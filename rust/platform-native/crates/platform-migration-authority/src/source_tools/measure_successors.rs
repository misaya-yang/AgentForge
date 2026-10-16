//! Explicitly approved, disposable PostgreSQL measurement for inactive schema successors.
//! Source and preview validation is read-only; writes are confined to the named isolated DB
//! and its external preview verification references.
use crate::{
    AuthorityError, DEFAULT_BASELINE_ID, Result, bootstrap, commands, credentials, fingerprint,
    ledger_acl, manifest,
    runner::{AuthorityPaths, MigrationAuthority},
    source_tools::successors,
};
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, Row};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    net::IpAddr,
    path::{Path, PathBuf},
};

const FIRST_SUCCESSOR: i32 = 18;
const LAST_SUCCESSOR: i32 = 20;
const EXPECTED_DB_PREFIX: &str = "schema_successor_verify_";
const LEDGER_ACL_DELTA: &str = "schema_acl:public:migrator:USAGE:grantopt=0";
const NATIVE_PRODUCER_DESCRIPTOR: &[u8] =
    include_bytes!("../../../../../../database/authority/native-producer-migration-v1.json");

fn refused(message: &str) -> AuthorityError {
    AuthorityError::Refused(message.into())
}

#[derive(Debug, Clone)]
pub struct MeasureOptions {
    pub source_root: PathBuf,
    pub preview_root: PathBuf,
    pub preimage_root: Option<PathBuf>,
    pub expected_database: String,
    pub run_approved: bool,
}

#[derive(Debug, Clone)]
struct SourceDistributionEvidence {
    archive_sha256: String,
    import_manifest_sha256: String,
    import_receipt_sha256: String,
    source_files: u64,
    rust_files: u64,
    producer_descriptor_sha256: String,
    producer_replacements_verified: u64,
    previous_checkout: PathBuf,
    historical_preimage_verified: bool,
}

impl SourceDistributionEvidence {
    fn json(&self) -> Value {
        json!({
            "archive_sha256": self.archive_sha256,
            "import_manifest_sha256": self.import_manifest_sha256,
            "import_receipt_sha256": self.import_receipt_sha256,
            "source_files": self.source_files,
            "rust_files": self.rust_files,
            "verified_source_files": self.source_files,
            "all_source_files_match_archive": true,
            "python_source_files_in_archive": 0,
            "producer_descriptor_sha256": self.producer_descriptor_sha256,
            "producer_replacements_verified": self.producer_replacements_verified,
            "historical_preimage_sha256": "9df9e3a5d6f572da0ea325720b9b346dac774055de8e9d900f3f82c2b55857fc",
            "historical_preimage_verified": self.historical_preimage_verified,
            "historical_git_blobs_verified": false
        })
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn source_distribution_evidence(source_root: &Path) -> Result<SourceDistributionEvidence> {
    let path = source_root.join("reports/dots-source-import-20261015.json");
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(refused(
            "verified dots source-import receipt must be a regular file",
        ));
    }
    let bytes = fs::read(&path)?;
    let receipt: Value = serde_json::from_slice(&bytes)
        .map_err(|_| refused("invalid dots source-import receipt"))?;
    let target = receipt["target"]
        .as_str()
        .ok_or_else(|| refused("source-import receipt has no target"))?;
    let target = Path::new(target)
        .canonicalize()
        .map_err(|_| refused("source-import receipt target is unavailable"))?;
    let source_files = receipt["source_files"]
        .as_u64()
        .ok_or_else(|| refused("source-import receipt has an invalid file count"))?;
    let rust_files = receipt["rust_files"]
        .as_u64()
        .ok_or_else(|| refused("source-import receipt has an invalid Rust count"))?;
    let verified_source_files = receipt["verified_source_files"]
        .as_u64()
        .ok_or_else(|| refused("source-import receipt has an invalid verified count"))?;
    let archive_sha256 = receipt["archive_sha256"]
        .as_str()
        .ok_or_else(|| refused("source-import receipt has no archive digest"))?;
    let source_archive = receipt["source_archive"]
        .as_str()
        .ok_or_else(|| refused("source-import receipt has no archive path"))?;
    let import_manifest_sha256 = receipt["manifest_sha256"]
        .as_str()
        .ok_or_else(|| refused("source-import receipt has no manifest digest"))?;
    let previous_checkout = receipt["previous_checkout"]
        .as_str()
        .ok_or_else(|| refused("source-import receipt has no preserved checkout"))?;
    if receipt["schema"] != "ai-platform/dots-source-import/v1"
        || target != source_root
        || source_files != 4_599
        || rust_files != 2_082
        || verified_source_files != source_files
        || receipt["all_source_files_match_archive"] != true
        || receipt["python_source_files_in_archive"] != 0
        || receipt["git_head_preserved"] != true
        || !is_sha256(archive_sha256)
        || !is_sha256(import_manifest_sha256)
    {
        return Err(refused(
            "dots source-import receipt does not match the verified source distribution",
        ));
    }
    let archive_path = Path::new(source_archive);
    let archive_metadata = fs::symlink_metadata(archive_path)?;
    if !archive_path.is_absolute()
        || archive_metadata.file_type().is_symlink()
        || !archive_metadata.is_file()
        || manifest::file_sha256(archive_path)? != archive_sha256
    {
        return Err(refused(
            "dots source archive does not match its verified import receipt",
        ));
    }
    let previous_checkout = canonical_directory(
        Path::new(previous_checkout),
        "preserved preimage checkout must be an existing nonsymlink directory",
    )?;
    Ok(SourceDistributionEvidence {
        archive_sha256: archive_sha256.to_owned(),
        import_manifest_sha256: import_manifest_sha256.to_owned(),
        import_receipt_sha256: manifest::file_sha256(&path)?,
        source_files,
        rust_files,
        producer_descriptor_sha256: String::new(),
        producer_replacements_verified: 0,
        previous_checkout,
        historical_preimage_verified: false,
    })
}

fn validate_preimage_root(
    supplied: Option<&Path>,
    source_distribution: &SourceDistributionEvidence,
    source_root: &Path,
    preview_root: &Path,
) -> Result<PathBuf> {
    let supplied = supplied.ok_or_else(|| {
        refused("--preimage-root is required for the missing frozen ledger ACL preimage")
    })?;
    let preimage_root = canonical_directory(
        supplied,
        "preimage root must be an existing nonsymlink directory",
    )?;
    if preimage_root != source_distribution.previous_checkout
        || preimage_root.starts_with(source_root)
        || source_root.starts_with(&preimage_root)
        || preimage_root.starts_with(preview_root)
        || preview_root.starts_with(&preimage_root)
    {
        return Err(refused(
            "preimage root must match the exact preserved checkout in the source-import receipt",
        ));
    }
    Ok(preimage_root)
}

fn source_file_without_symlinks(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(refused(
            "producer descriptor contains an unsafe source path",
        ));
    }
    let mut path = root.to_path_buf();
    for part in relative_path.components() {
        path.push(part.as_os_str());
        if fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err(refused(
                "producer descriptor source path contains a symlink",
            ));
        }
    }
    let path = path.canonicalize()?;
    if !path.starts_with(root) || !path.is_file() {
        return Err(refused("producer descriptor source file is unavailable"));
    }
    Ok(path)
}

pub(crate) fn verify_native_producer_descriptor(
    source_root: &Path,
    baseline: &manifest::BaselineManifest,
    baseline_sha: &str,
) -> Result<(String, u64)> {
    let descriptor_path = source_file_without_symlinks(
        source_root,
        "database/authority/native-producer-migration-v1.json",
    )?;
    let bytes = fs::read(&descriptor_path)?;
    if bytes != NATIVE_PRODUCER_DESCRIPTOR {
        return Err(refused(
            "native producer migration descriptor differs from its compiled identity",
        ));
    }
    let descriptor: Value = serde_json::from_slice(&bytes)
        .map_err(|_| refused("invalid native producer migration descriptor"))?;
    if descriptor["schema"] != "native-authority-producer-migration/v1"
        || descriptor["baseline_id"] != baseline.baseline_id
        || descriptor["baseline_manifest_sha256"] != baseline_sha
        || descriptor["source_git_sha"] != baseline.source_git_sha
    {
        return Err(refused(
            "native producer descriptor does not bind the frozen baseline",
        ));
    }
    let retired = descriptor["retired"]
        .as_array()
        .ok_or_else(|| refused("native producer descriptor lacks retired sources"))?;
    if retired.len() != 7 {
        return Err(refused(
            "native producer descriptor has an unexpected retired-source set",
        ));
    }
    let mut replacements_verified = 0u64;
    for entry in retired {
        let historical_path = entry["path"]
            .as_str()
            .ok_or_else(|| refused("invalid retired producer path"))?;
        let historical = Path::new(historical_path);
        if historical.is_absolute()
            || historical
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(refused("unsafe retired producer path"));
        }
        let mut historical_path = source_root.to_path_buf();
        let mut missing = false;
        for component in historical.components() {
            historical_path.push(component.as_os_str());
            match fs::symlink_metadata(&historical_path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(refused("retired producer path contains a symlink"));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    missing = true;
                    break;
                }
                Err(error) => return Err(error.into()),
            }
        }
        if !missing {
            return Err(refused(
                "retired Python producer is present in the source distribution",
            ));
        }
        let replacements = entry["replacements"]
            .as_object()
            .filter(|items| !items.is_empty())
            .ok_or_else(|| refused("native producer descriptor has no replacements"))?;
        for (relative, expected) in replacements {
            let path = source_file_without_symlinks(source_root, relative)?;
            if manifest::file_sha256(&path)? != expected.as_str().unwrap_or("") {
                return Err(refused(
                    "native producer replacement differs from its descriptor hash",
                ));
            }
            replacements_verified += 1;
        }
    }
    Ok((
        manifest::file_sha256(&descriptor_path)?,
        replacements_verified,
    ))
}

fn load_snapshot_baseline(paths: &AuthorityPaths) -> Result<(manifest::BaselineManifest, String)> {
    let baseline_path = paths
        .baseline_dir(DEFAULT_BASELINE_ID)
        .join("manifest.json");
    let baseline = manifest::load_baseline_manifest(&baseline_path)?;
    let legacy =
        manifest::load_legacy_manifest(&paths.migrations_root().join("legacy-manifest.yml"))?;
    if legacy.freeze_point != baseline.last_legacy_change {
        return Err(refused(
            "baseline freeze point disagrees with the immutable legacy manifest",
        ));
    }
    Ok((baseline, manifest::file_sha256(&baseline_path)?))
}

pub fn validate_database_name(name: &str) -> Result<()> {
    let suffix = name.strip_prefix(EXPECTED_DB_PREFIX).unwrap_or("");
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

#[derive(Debug)]
pub(crate) struct SingleStepEvidence {
    pub applied: BTreeMap<i32, String>,
    pub raw_fingerprints: BTreeMap<String, String>,
    pub base_acl_sha256: String,
    pub raw_acl_sha256: String,
    pub raw_acl_lines: Vec<String>,
    pub base_acl_lines: Vec<String>,
}

/// Apply exactly one reviewed extension in a named disposable database, measure it,
/// and prove replay is a no-op. Both the 020->021 and 021->022 source paths use this
/// same migrator-role, loopback, advisory-lock, and ledger-prefix runner.
pub(crate) async fn measure_single_step<F>(
    preview_database: &Path,
    expected_database: &str,
    predecessor_epoch: i32,
    candidate_epoch: i32,
    baseline: &manifest::BaselineManifest,
    baseline_sha256: &str,
    epoch: &manifest::EpochManifest,
    run_approved: bool,
    write_verification_reference: F,
) -> Result<SingleStepEvidence>
where
    F: FnOnce(&SingleStepEvidence) -> Result<()>,
{
    if !run_approved {
        return Err(refused(
            "measurement requires --run-approved-isolated-measurements",
        ));
    }
    validate_database_name(expected_database)?;
    if crate::distribution::SourceMode::from_process()?
        != crate::distribution::SourceMode::Development
    {
        return Err(refused(
            "isolated successor measurement requires AI_GATEWAY_AUTHORITY_SOURCE_MODE=development",
        ));
    }
    if candidate_epoch != predecessor_epoch + 1
        || epoch.epoch != candidate_epoch
        || epoch.baseline_id != baseline.baseline_id
        || epoch.changes.len() != candidate_epoch as usize
        || epoch
            .changes
            .iter()
            .enumerate()
            .any(|(index, change)| change.sequence != index as i32 + 1)
    {
        return Err(refused(
            "single-step measurement requires one exact contiguous successor",
        ));
    }

    let role_prefix = credentials::role_prefix_from_env()?;
    let migrator = format!("{role_prefix}migrator");
    let dsn = credentials::dsn_for_principal("migrator")?;
    validate_loopback_dsn(&dsn, expected_database, &migrator)?;
    let paths = AuthorityPaths {
        database_dir: preview_database.to_path_buf(),
    };
    let authority = MigrationAuthority::new(dsn, paths.clone(), role_prefix.clone())?;
    let mut lock = authority.connect(false).await?;
    validate_database_session(&mut lock, expected_database, &migrator).await?;
    authority.acquire_lock(&mut lock).await?;

    let mut write_verification_reference = Some(write_verification_reference);
    let outcome = async {
        let mut conn = authority.connect(false).await?;
        validate_database_session(&mut conn, expected_database, &migrator).await?;
        if !bootstrap::role_bootstrap_issues(&mut conn, &role_prefix)
            .await?
            .is_empty()
        {
            return Err(refused(
                "isolated database authority roles are not in the exact safe state",
            ));
        }
        if !ledger_acl::inspect(&mut conn, &role_prefix).await? {
            return Err(refused(
                "isolated database lacks the exact versioned ledger ACL",
            ));
        }
        let marker = authority
            .adopted_baseline(&mut conn)
            .await?
            .ok_or_else(|| refused("isolated database has no adopted baseline marker"))?;
        bootstrap::validate_adoption_marker(&marker, baseline, baseline_sha256)?;
        let mut applied = authority
            .applied_changes(&mut conn, &baseline.baseline_id)
            .await?;
        let expected = epoch
            .changes
            .iter()
            .map(|change| (change.sequence, change.sha256.clone()))
            .collect::<BTreeMap<_, _>>();
        if !exact_change_prefix(&expected, &applied, predecessor_epoch) {
            return Err(refused(
                "isolated database must contain the exact complete approved predecessor prefix",
            ));
        }
        commands::require_ledger_state(&authority, &mut conn, baseline).await?;
        commands::verify(&authority, &baseline.baseline_id).await?;

        let epoch_dir = paths.epoch_dir(&baseline.baseline_id);
        let change = epoch
            .changes
            .get(candidate_epoch as usize - 1)
            .filter(|change| change.sequence == candidate_epoch)
            .ok_or_else(|| refused("validated successor manifest lost its single extension"))?;
        authority
            .apply_change_transactional(&mut conn, &baseline.baseline_id, change, &epoch_dir)
            .await?;
        applied = authority
            .applied_changes(&mut conn, &baseline.baseline_id)
            .await?;
        if !exact_change_prefix(&expected, &applied, candidate_epoch) {
            return Err(refused(
                "single-step application did not produce the exact complete ledger prefix",
            ));
        }
        let (raw_fingerprints, base_acl_sha256, raw_acl_sha256, raw_acl_lines, base_acl_lines) =
            measured_fingerprints(&mut conn, &authority, baseline).await?;
        let evidence = SingleStepEvidence {
            applied: applied.clone(),
            raw_fingerprints,
            base_acl_sha256,
            raw_acl_sha256,
            raw_acl_lines,
            base_acl_lines,
        };
        let write_reference = write_verification_reference
            .take()
            .ok_or_else(|| refused("measurement reference callback was already used"))?;
        write_reference(&evidence)?;
        commands::require_ledger_state(&authority, &mut conn, baseline).await?;
        commands::verify(&authority, &baseline.baseline_id).await?;
        let replay = authority.apply_epoch(&mut conn, epoch, &epoch_dir).await?;
        if !replay.is_empty() {
            return Err(refused("measured successor replay was not a no-op"));
        }
        commands::require_ledger_state(&authority, &mut conn, baseline).await?;
        commands::verify(&authority, &baseline.baseline_id).await?;
        conn.close().await?;
        Ok(evidence)
    }
    .await;
    let released = authority.release_lock(lock).await;
    match (outcome, released) {
        (Ok(evidence), Ok(())) => Ok(evidence),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

pub(crate) fn exact_change_prefix(
    expected: &BTreeMap<i32, String>,
    applied: &BTreeMap<i32, String>,
    epoch: i32,
) -> bool {
    expected.len() >= epoch as usize
        && applied.len() == epoch as usize
        && applied.keys().copied().eq(1..=epoch)
        && (1..=epoch).all(|sequence| applied.get(&sequence) == expected.get(&sequence))
}

fn require_explicit_approval(approved: bool) -> Result<()> {
    if approved {
        Ok(())
    } else {
        Err(refused(
            "measurement requires --run-approved-isolated-measurements",
        ))
    }
}

fn require_development_source_mode(mode: crate::distribution::SourceMode) -> Result<()> {
    if mode == crate::distribution::SourceMode::Development {
        Ok(())
    } else {
        Err(refused(
            "isolated successor measurement requires AI_GATEWAY_AUTHORITY_SOURCE_MODE=development",
        ))
    }
}

fn is_loopback_host(host: &str) -> bool {
    let host = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// Require a PostgreSQL URI that names only an explicit loopback host. Only `sslmode` is
/// accepted as a query parameter so libpq-style host/service/port overrides cannot redirect it.
pub(crate) fn validate_loopback_dsn(
    dsn: &str,
    expected_database: &str,
    expected_user: &str,
) -> Result<()> {
    let url = url::Url::parse(dsn)
        .map_err(|_| refused("migrator DSN must be a loopback PostgreSQL URI"))?;
    if !matches!(url.scheme(), "postgres" | "postgresql")
        || url.host_str().is_none_or(|host| !is_loopback_host(host))
        || url.fragment().is_some()
        || url.username() != expected_user
    {
        return Err(refused(
            "migrator DSN must target the local isolated database",
        ));
    }
    for (key, value) in url.query_pairs() {
        if key != "sslmode"
            || !matches!(
                value.as_ref(),
                "disable" | "allow" | "prefer" | "require" | "verify-ca" | "verify-full"
            )
        {
            return Err(refused(
                "migrator DSN has unsupported routing or connection parameters",
            ));
        }
    }
    let database = url.path().trim_start_matches('/');
    if database.contains('/') || database != expected_database {
        return Err(refused(
            "migrator DSN database path does not match the isolated target",
        ));
    }
    Ok(())
}

fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf> {
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

fn verify_plain_tree(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(refused("successor preview must not contain symlinks"));
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            verify_plain_tree(&entry?.path())?;
        }
    } else if !metadata.is_file() {
        return Err(refused(
            "successor preview contains an unsupported file type",
        ));
    }
    Ok(())
}

fn validate_roots(source: &Path, preview: &Path) -> Result<(PathBuf, PathBuf)> {
    let source = canonical_directory(
        source,
        "source root must be an existing nonsymlink directory",
    )?;
    let preview = canonical_directory(
        preview,
        "preview root must be an existing nonsymlink directory",
    )?;
    if source.starts_with(&preview) || preview.starts_with(&source) {
        return Err(refused(
            "successor preview must be external to the source root",
        ));
    }
    if preview.join(".git").exists() {
        return Err(refused(
            "external successor preview must not contain Git metadata",
        ));
    }
    verify_plain_tree(&preview)?;
    Ok((source, preview))
}

fn validate_source_preview(
    source_root: &Path,
    preview_root: &Path,
    preimage_root: &Path,
) -> Result<(
    manifest::BaselineManifest,
    String,
    manifest::EpochManifest,
    Value,
)> {
    let source_paths = AuthorityPaths {
        database_dir: source_root.join("database"),
    };
    let preview_paths = AuthorityPaths {
        database_dir: preview_root.join("database"),
    };
    let (source_baseline, source_baseline_sha) = commands::load_baseline_with_mode(
        &source_paths,
        DEFAULT_BASELINE_ID,
        crate::distribution::SourceMode::Distribution,
    )?;
    let (preview_baseline, preview_baseline_sha) = load_snapshot_baseline(&preview_paths)?;
    if source_baseline_sha != preview_baseline_sha {
        return Err(refused(
            "preview baseline differs from the validated source baseline",
        ));
    }
    let source_epoch = manifest::load_epoch_manifest(
        &source_paths
            .epoch_dir(DEFAULT_BASELINE_ID)
            .join("manifest.yml"),
    )?;
    if source_epoch.epoch != 17 || source_epoch.changes.len() != 17 {
        return Err(refused(
            "source epoch is not the frozen 17-change predecessor",
        ));
    }

    let (plan, preview_epoch) = successors::verify_preview_with_preimage_root(
        source_root,
        preview_root,
        Some(preimage_root),
    )?;
    if preview_epoch.baseline_id != DEFAULT_BASELINE_ID
        || preview_epoch.epoch != LAST_SUCCESSOR
        || preview_epoch.changes.len() != LAST_SUCCESSOR as usize
        || serde_json::to_value(&source_epoch.changes)
            .map_err(|_| refused("cannot bind source epoch"))?
            != serde_json::to_value(&preview_epoch.changes[..17])
                .map_err(|_| refused("cannot bind preview predecessor"))?
        || source_baseline.baseline_id != preview_baseline.baseline_id
    {
        return Err(refused(
            "preview is not the exact source-bound 20-change successor set",
        ));
    }
    let planned = plan["successors"]
        .as_array()
        .ok_or_else(|| refused("verified successor plan has no entries"))?;
    if planned.len() != 3 {
        return Err(refused(
            "verified successor plan has an unexpected allocation",
        ));
    }
    for (index, entry) in planned.iter().enumerate() {
        let spec = &preview_epoch.changes[17 + index];
        let change = &entry["change"];
        if spec.sequence != FIRST_SUCCESSOR + index as i32
            || change["sequence"] != spec.sequence
            || change["name"] != spec.name
            || change["file"] != spec.file
            || change["sha256"] != spec.sha256
        {
            return Err(refused(
                "preview successor SQL does not match its validated source plan",
            ));
        }
    }
    let epoch_dir = preview_paths.epoch_dir(DEFAULT_BASELINE_ID);
    for sequence in FIRST_SUCCESSOR..=LAST_SUCCESSOR {
        match fs::symlink_metadata(epoch_dir.join(format!("{sequence:03}_verification.json"))) {
            Ok(_) => return Err(refused("successor measurement reference already exists")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    match fs::symlink_metadata(preview_root.join("SUCCESSOR_MEASUREMENT.json")) {
        Ok(_) => return Err(refused("successor measurement receipt already exists")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok((source_baseline, source_baseline_sha, preview_epoch, plan))
}

pub(crate) async fn validate_database_session(
    conn: &mut PgConnection,
    expected_database: &str,
    expected_migrator: &str,
) -> Result<()> {
    let row = sqlx::query(
        "SELECT current_database()::text AS database_name, current_user::text AS current_role, \
         session_user::text AS session_role, current_setting('server_version_num')::integer AS version_num, \
         (SELECT r.rolsuper FROM pg_catalog.pg_roles r WHERE r.rolname=session_user) AS is_superuser, \
         (SELECT r.rolcanlogin FROM pg_catalog.pg_roles r WHERE r.rolname=session_user) AS can_login",
    )
    .fetch_one(&mut *conn)
    .await?;
    let database: String = row.try_get("database_name")?;
    let current_role: String = row.try_get("current_role")?;
    let session_role: String = row.try_get("session_role")?;
    let version: i32 = row.try_get("version_num")?;
    let superuser: bool = row.try_get("is_superuser")?;
    let can_login: bool = row.try_get("can_login")?;
    if database != expected_database
        || current_role != expected_migrator
        || session_role != expected_migrator
        || superuser
        || !can_login
        || !(160_000..170_000).contains(&version)
    {
        return Err(refused(
            "connection must be the exact non-superuser migrator role on PostgreSQL 16 and the isolated database",
        ));
    }
    Ok(())
}

pub(crate) fn acl_reference(
    raw: &BTreeMap<String, String>,
    lines: &[String],
) -> Result<(String, String, Vec<String>)> {
    let raw_acl = fingerprint::canonical_digest(lines);
    if raw.get("acl") != Some(&raw_acl) {
        return Err(refused(
            "raw ACL lines do not match the catalog fingerprint",
        ));
    }
    let delta_count = lines
        .iter()
        .filter(|line| line.as_str() == LEDGER_ACL_DELTA)
        .count();
    if delta_count != 1 {
        return Err(refused(
            "ledger ACL v1 public migrator USAGE delta is absent, duplicated or altered",
        ));
    }
    let base_lines = lines
        .iter()
        .filter(|line| line.as_str() != LEDGER_ACL_DELTA)
        .cloned()
        .collect::<Vec<_>>();
    let base_acl = fingerprint::canonical_digest(&base_lines);
    Ok((base_acl, raw_acl, base_lines))
}

pub(crate) async fn measured_fingerprints(
    conn: &mut PgConnection,
    authority: &MigrationAuthority,
    baseline: &manifest::BaselineManifest,
) -> Result<(
    BTreeMap<String, String>,
    String,
    String,
    Vec<String>,
    Vec<String>,
)> {
    let mut tx = conn.begin().await?;
    let previous_role: String = sqlx::query_scalar("SELECT current_setting('role')")
        .fetch_one(&mut *tx)
        .await?;
    let previous_search: String = sqlx::query_scalar("SELECT current_setting('search_path')")
        .fetch_one(&mut *tx)
        .await?;
    let result = async {
        crate::runner::set_role(&mut tx, &format!("{}owner", authority.role_prefix)).await?;
        sqlx::query("SELECT set_config('search_path','pg_catalog',true)")
            .execute(&mut *tx)
            .await?;
        if !ledger_acl::inspect(&mut tx, &authority.role_prefix).await? {
            return Err(refused("versioned ledger ACL is not installed"));
        }
        let raw = fingerprint::compute_fingerprints(
            &mut tx,
            &authority.role_prefix,
            &baseline.reference_data,
        )
        .await?;
        let lines = fingerprint::acl_lines(&mut tx, &authority.role_prefix).await?;
        let (base_acl, raw_acl, base_lines) = acl_reference(&raw, &lines)?;
        let mut base = raw.clone();
        base.insert("acl".into(), base_acl.clone());
        let effective =
            ledger_acl::compatible_expected(&mut tx, &base, &authority.role_prefix).await?;
        if effective != raw || effective.get("acl") != Some(&raw_acl) {
            return Err(refused(
                "ledger ACL v1 compatibility did not reconstruct the raw measured fingerprints",
            ));
        }
        Ok((raw, base_acl, raw_acl, lines, base_lines))
    }
    .await;
    let reset = async {
        sqlx::query("SELECT set_config('search_path',$1,true)")
            .bind(previous_search)
            .execute(&mut *tx)
            .await?;
        crate::runner::set_role(&mut tx, &previous_role).await?;
        tx.rollback().await?;
        Ok::<(), crate::AuthorityError>(())
    }
    .await;
    match (result, reset) {
        (Ok(values), Ok(())) => Ok(values),
        (Err(error), _) => Err(error),
        (_, Err(error)) => Err(error),
    }
}

fn verification_reference(
    baseline: &manifest::BaselineManifest,
    baseline_sha: &str,
    sequence: i32,
    applied: &BTreeMap<i32, String>,
    raw: &BTreeMap<String, String>,
    base_acl: &str,
    raw_acl: &str,
    acl_lines: &[String],
    base_acl_lines: &[String],
    source_distribution: &SourceDistributionEvidence,
) -> Result<Value> {
    let mut fingerprints = raw.clone();
    fingerprints.insert("acl".into(), base_acl.into());
    let changes = applied
        .iter()
        .map(|(sequence, checksum)| (sequence.to_string(), json!(checksum)))
        .collect::<serde_json::Map<_, _>>();
    let source_provenance = "Baseline Git ancestry was not used; the exact dots source-import receipt and frozen manifests/preimages bind this source snapshot.";
    Ok(json!({
        "baseline_id": baseline.baseline_id,
        "sequence": sequence,
        "baseline_manifest_sha256": baseline_sha,
        "changes": changes,
        "fingerprints": fingerprints,
        "raw_fingerprints": raw,
        "acl_compatibility": {
            "revision": ledger_acl::REVISION,
            "manifest_sha256": ledger_acl::MANIFEST_SHA256,
            "delta_line": LEDGER_ACL_DELTA,
            "base_acl_sha256": base_acl,
            "effective_acl_sha256": raw_acl,
            "removed_delta_count": 1,
            "raw_acl_lines": acl_lines,
            "base_acl_lines": base_acl_lines
        },
        "provenance": format!(
            "Explicit isolated PostgreSQL 16 measurement with the native authority runner. {source_provenance}"
        ),
        "provenance_details": {
            "tool": "rust/platform-native/crates/platform-migration-authority/src/source_tools/measure_successors.rs",
            "git_baseline_provenance_verified": false,
            "source_distribution": source_distribution.json(),
            "measurement": "explicitly approved isolated PostgreSQL 16 database"
        }
    }))
}

fn write_reference(path: &Path, value: &Value) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| refused("invalid verification reference destination"))?;
    let parent_meta = fs::symlink_metadata(parent)?;
    if parent_meta.file_type().is_symlink() || !parent_meta.is_dir() {
        return Err(refused(
            "verification reference directory must be nonsymlink",
        ));
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    serde_json::to_writer_pretty(&mut file, value)
        .map_err(|_| refused("verification reference serialization failed"))?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn write_measurement_receipt(
    source_root: &Path,
    preview_root: &Path,
    expected_database: &str,
    source_plan_sha: &str,
    baseline_sha: &str,
    epoch_manifest_sha: &str,
    source_distribution: &SourceDistributionEvidence,
) -> Result<()> {
    if manifest::file_sha256(&source_root.join("docs/design/canonical-schema-successors.json"))?
        != source_plan_sha
        || manifest::file_sha256(&source_root.join("reports/dots-source-import-20261015.json"))?
            != source_distribution.import_receipt_sha256
    {
        return Err(refused("source import evidence changed during measurement"));
    }
    let epoch_dir = preview_root
        .join("database/migrations")
        .join(DEFAULT_BASELINE_ID);
    if manifest::file_sha256(&epoch_dir.join("manifest.yml"))? != epoch_manifest_sha
        || manifest::file_sha256(
            &preview_root
                .join("database/baselines")
                .join(DEFAULT_BASELINE_ID)
                .join("manifest.json"),
        )? != baseline_sha
    {
        return Err(refused(
            "successor source artifacts changed during measurement",
        ));
    }
    let mut refs = serde_json::Map::new();
    for sequence in FIRST_SUCCESSOR..=LAST_SUCCESSOR {
        let name = format!("{sequence:03}_verification.json");
        let path = epoch_dir.join(&name);
        let value: Value = serde_json::from_slice(&fs::read(&path)?)
            .map_err(|_| refused("invalid measured successor verification reference"))?;
        if value["sequence"] != sequence
            || value["baseline_id"] != DEFAULT_BASELINE_ID
            || value["baseline_manifest_sha256"] != baseline_sha
        {
            return Err(refused("successor verification reference binding mismatch"));
        }
        refs.insert(name, json!(manifest::file_sha256(&path)?));
    }
    let receipt = json!({
        "schema": "ai-platform/canonical-successor-measurement/v1",
        "status": "measured_external_preview",
        "database_actions": true,
        "database_actions_scope": "named_isolated_database_only",
        "source_activation": false,
        "expected_database": expected_database,
        "source_plan_sha256": source_plan_sha,
        "baseline_manifest_sha256": baseline_sha,
        "epoch_manifest_sha256": epoch_manifest_sha,
        "successor_sequences": [18, 19, 20],
        "verification_refs": refs,
        "post_migration_noop": true,
        "source_distribution": source_distribution.json()
    });
    write_reference(&preview_root.join("SUCCESSOR_MEASUREMENT.json"), &receipt)
}

async fn measure(options: MeasureOptions) -> Result<()> {
    require_explicit_approval(options.run_approved)?;
    validate_database_name(&options.expected_database)?;
    require_development_source_mode(crate::distribution::SourceMode::from_process()?)?;
    let (source_root, preview_root) = validate_roots(&options.source_root, &options.preview_root)?;
    let mut source_distribution = source_distribution_evidence(&source_root)?;
    let preimage_root = validate_preimage_root(
        options.preimage_root.as_deref(),
        &source_distribution,
        &source_root,
        &preview_root,
    )?;
    let (baseline, baseline_sha, epoch, _plan) =
        validate_source_preview(&source_root, &preview_root, &preimage_root)?;
    source_distribution.historical_preimage_verified = true;
    let (producer_descriptor_sha, producer_replacements_verified) =
        verify_native_producer_descriptor(&source_root, &baseline, &baseline_sha)?;
    source_distribution.producer_descriptor_sha256 = producer_descriptor_sha;
    source_distribution.producer_replacements_verified = producer_replacements_verified;
    let preview_paths = AuthorityPaths {
        database_dir: preview_root.join("database"),
    };
    let (_, current_baseline_sha) = load_snapshot_baseline(&preview_paths)?;
    if current_baseline_sha != baseline_sha {
        return Err(refused("preview baseline changed during preflight"));
    }
    let source_plan_sha =
        manifest::file_sha256(&source_root.join("docs/design/canonical-schema-successors.json"))?;
    let epoch_manifest_sha = manifest::file_sha256(
        &preview_paths
            .epoch_dir(DEFAULT_BASELINE_ID)
            .join("manifest.yml"),
    )?;
    let role_prefix = credentials::role_prefix_from_env()?;
    let migrator = format!("{role_prefix}migrator");
    let dsn = credentials::dsn_for_principal("migrator")?;
    validate_loopback_dsn(&dsn, &options.expected_database, &migrator)?;
    let authority = MigrationAuthority::new(dsn, preview_paths.clone(), role_prefix.clone())?;

    let mut lock = authority.connect(false).await?;
    validate_database_session(&mut lock, &options.expected_database, &migrator).await?;
    authority.acquire_lock(&mut lock).await?;
    let outcome = async {
        let mut conn = authority.connect(false).await?;
        validate_database_session(&mut conn, &options.expected_database, &migrator).await?;
        if !bootstrap::role_bootstrap_issues(&mut conn, &role_prefix)
            .await?
            .is_empty()
        {
            return Err(refused(
                "isolated database authority roles are not in the exact safe state",
            ));
        }
        if !ledger_acl::inspect(&mut conn, &role_prefix).await? {
            return Err(refused(
                "isolated database lacks the exact versioned ledger ACL",
            ));
        }
        let marker = authority
            .adopted_baseline(&mut conn)
            .await?
            .ok_or_else(|| refused("isolated database has no adopted baseline marker"))?;
        bootstrap::validate_adoption_marker(&marker, &baseline, &baseline_sha)?;
        let applied = authority
            .applied_changes(&mut conn, DEFAULT_BASELINE_ID)
            .await?;
        crate::runner::validate_applied_prefix(&epoch, &applied)?;
        if applied.len() != 17
            || applied.keys().copied().ne(1..=17)
            || epoch
                .changes
                .iter()
                .take(17)
                .any(|spec| applied.get(&spec.sequence) != Some(&spec.sha256))
        {
            return Err(refused(
                "isolated database must contain the exact complete predecessor ledger 1 through 17",
            ));
        }
        commands::require_ledger_state(&authority, &mut conn, &baseline).await?;
        commands::verify(&authority, DEFAULT_BASELINE_ID).await?;

        let epoch_dir = preview_paths.epoch_dir(DEFAULT_BASELINE_ID);
        for sequence in FIRST_SUCCESSOR..=LAST_SUCCESSOR {
            let current = authority
                .applied_changes(&mut conn, DEFAULT_BASELINE_ID)
                .await?;
            if current.len() != (sequence - 1) as usize
                || current.keys().copied().ne(1..sequence)
                || current.iter().any(|(key, checksum)| {
                    epoch.changes[(key - 1) as usize].sha256.as_str() != checksum.as_str()
                })
            {
                return Err(refused(
                    "isolated ledger changed outside the expected successor prefix",
                ));
            }
            let spec = epoch
                .changes
                .get((sequence - 1) as usize)
                .filter(|spec| spec.sequence == sequence)
                .ok_or_else(|| refused("validated successor manifest lost its sequence"))?;
            authority
                .apply_change_transactional(&mut conn, DEFAULT_BASELINE_ID, spec, &epoch_dir)
                .await?;
            let measured_applied = authority
                .applied_changes(&mut conn, DEFAULT_BASELINE_ID)
                .await?;
            if measured_applied.len() != sequence as usize
                || measured_applied.keys().copied().ne(1..=sequence)
                || measured_applied.get(&sequence) != Some(&spec.sha256)
            {
                return Err(refused(
                    "successor application did not produce its exact ledger prefix",
                ));
            }
            let (raw, base_acl, raw_acl, acl_lines, base_acl_lines) =
                measured_fingerprints(&mut conn, &authority, &baseline).await?;
            let reference = verification_reference(
                &baseline,
                &baseline_sha,
                sequence,
                &measured_applied,
                &raw,
                &base_acl,
                &raw_acl,
                &acl_lines,
                &base_acl_lines,
                &source_distribution,
            )?;
            write_reference(
                &epoch_dir.join(format!("{sequence:03}_verification.json")),
                &reference,
            )?;
            commands::require_ledger_state(&authority, &mut conn, &baseline).await?;
            commands::verify(&authority, DEFAULT_BASELINE_ID).await?;
        }

        let noop = authority.apply_epoch(&mut conn, &epoch, &epoch_dir).await?;
        if !noop.is_empty() {
            return Err(refused("fully measured successor epoch was not a no-op"));
        }
        commands::require_ledger_state(&authority, &mut conn, &baseline).await?;
        commands::verify(&authority, DEFAULT_BASELINE_ID).await?;
        conn.close().await?;
        let _ = successors::validate_with_preimage_root(&source_root, Some(&preimage_root))?;
        let mut final_distribution = source_distribution_evidence(&source_root)?;
        if final_distribution.previous_checkout != preimage_root {
            return Err(refused(
                "preserved preimage checkout changed during measurement",
            ));
        }
        let (final_descriptor_sha, final_replacements) =
            verify_native_producer_descriptor(&source_root, &baseline, &baseline_sha)?;
        final_distribution.producer_descriptor_sha256 = final_descriptor_sha;
        final_distribution.producer_replacements_verified = final_replacements;
        final_distribution.historical_preimage_verified = true;
        if final_distribution.json() != source_distribution.json() {
            return Err(refused(
                "source distribution identity changed during measurement",
            ));
        }
        write_measurement_receipt(
            &source_root,
            &preview_root,
            &options.expected_database,
            &source_plan_sha,
            &baseline_sha,
            &epoch_manifest_sha,
            &source_distribution,
        )?;
        successors::verify_measured_preview_with_preimage_root(
            &source_root,
            &preview_root,
            Some(&preimage_root),
        )?;
        Ok(())
    }
    .await;
    let released = authority.release_lock(lock).await;
    outcome.and(released)
}

pub async fn run(options: MeasureOptions) -> Result<()> {
    measure(options).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exact_isolated_database_names_are_accepted() {
        for name in [
            "schema_successor_verify_abcdefgh",
            "schema_successor_verify_0123456789abcdef0123456789abcdef0123456789abcdef",
        ] {
            assert!(validate_database_name(name).is_ok());
        }
        for name in [
            "schema_successor_verify_short",
            "schema_successor_verify_abcdefghX",
            "schema_successor_verify_abcdefgh-",
            "schema_successor_verify_abcdefgh;drop_database",
            "postgres",
        ] {
            assert!(validate_database_name(name).is_err());
        }
    }

    #[test]
    fn measurement_is_closed_without_the_explicit_approval_flag() {
        assert!(require_explicit_approval(false).is_err());
        assert!(require_explicit_approval(true).is_ok());
    }

    #[test]
    fn measurement_requires_development_mode_before_preview_database_work() {
        assert!(
            require_development_source_mode(crate::distribution::SourceMode::Development).is_ok()
        );
        assert!(
            require_development_source_mode(crate::distribution::SourceMode::Distribution).is_err()
        );
    }

    #[test]
    fn the_missing_historical_preimage_requires_an_explicit_reference_root() {
        let evidence = SourceDistributionEvidence {
            archive_sha256: String::new(),
            import_manifest_sha256: String::new(),
            import_receipt_sha256: String::new(),
            source_files: 4_599,
            rust_files: 2_082,
            producer_descriptor_sha256: String::new(),
            producer_replacements_verified: 0,
            previous_checkout: PathBuf::new(),
            historical_preimage_verified: false,
        };
        assert!(
            validate_preimage_root(None, &evidence, Path::new("/source"), Path::new("/preview"))
                .is_err()
        );
    }

    #[test]
    fn dsn_validation_rejects_redirects_and_remote_hosts_without_leaking_credentials() {
        assert!(is_loopback_host("[::1]"));
        assert!(!is_loopback_host("[::2]"));
        assert!(
            validate_loopback_dsn(
                "postgresql://ai_gateway_migrator:secret@localhost/db?host=remote.invalid",
                "db",
                "ai_gateway_migrator"
            )
            .is_err()
        );
        assert!(
            validate_loopback_dsn(
                "postgresql://ai_gateway_migrator:secret@db.example/db",
                "db",
                "ai_gateway_migrator"
            )
            .is_err()
        );
        assert!(
            validate_loopback_dsn(
                "postgresql://ai_gateway_migrator:secret@[::1]/db",
                "db",
                "ai_gateway_migrator"
            )
            .is_ok()
        );
        assert!(
            validate_loopback_dsn(
                "postgresql://ai_gateway_migrator:secret@127.0.0.1/db?sslmode=disable",
                "db",
                "ai_gateway_migrator"
            )
            .is_ok()
        );
        assert!(
            validate_loopback_dsn(
                "postgresql://ai_gateway_migrator:secret@127.0.0.1/other",
                "db",
                "ai_gateway_migrator"
            )
            .is_err()
        );
        assert!(
            validate_loopback_dsn(
                "postgresql://ai_gateway_migrator:secret@127.0.0.1",
                "db",
                "ai_gateway_migrator"
            )
            .is_err()
        );
        assert!(
            validate_loopback_dsn(
                "postgresql://ai_gateway_gateway:secret@127.0.0.1/db",
                "db",
                "ai_gateway_migrator"
            )
            .is_err()
        );
    }

    #[test]
    fn measured_base_acl_removes_only_the_exact_v1_delta_and_keeps_new_grants() {
        let lines = vec![
            "schema_acl:public:migrator:USAGE:grantopt=0".to_owned(),
            "function_acl:gateway.f():runtime:EXECUTE:grantopt=0".to_owned(),
            "acl:gateway.services:gateway:SELECT:grantopt=0".to_owned(),
        ];
        let mut raw = BTreeMap::from([
            ("structural".to_owned(), "1".to_owned()),
            ("acl".to_owned(), fingerprint::canonical_digest(&lines)),
            ("extensions".to_owned(), "2".to_owned()),
            ("reference_data".to_owned(), "3".to_owned()),
        ]);
        let (base, effective, base_lines) = acl_reference(&raw, &lines).unwrap();
        assert_eq!(base_lines.len(), 2);
        assert!(
            base_lines
                .iter()
                .any(|line| line.starts_with("function_acl:"))
        );
        assert_eq!(effective, raw["acl"]);
        assert_ne!(base, effective);
        raw.insert("acl".into(), base);
        assert!(acl_reference(&raw, &lines).is_err());
    }

    #[test]
    fn acl_v1_delta_must_appear_exactly_once() {
        let duplicate = vec![LEDGER_ACL_DELTA.to_owned(), LEDGER_ACL_DELTA.to_owned()];
        let raw = BTreeMap::from([("acl".to_owned(), fingerprint::canonical_digest(&duplicate))]);
        assert!(acl_reference(&raw, &duplicate).is_err());
        let missing = vec!["schema_acl:public:migrator:CREATE:grantopt=0".to_owned()];
        let raw = BTreeMap::from([("acl".to_owned(), fingerprint::canonical_digest(&missing))]);
        assert!(acl_reference(&raw, &missing).is_err());
    }
}
