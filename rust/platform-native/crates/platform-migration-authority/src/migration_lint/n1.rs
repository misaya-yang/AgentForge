use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use super::{
    CandidateSource, N1Receipt, N1RunOptions, Result, candidate_source, canonical_source,
    lint_source, refused,
};

const N1_DRIVER_TIMEOUT: Duration = Duration::from_secs(900);
const MAX_DRIVER_STREAM_BYTES: usize = 1024 * 1024;
const RECEIPT_SCHEMA: &str = "ai-platform/migration-n1-compatibility-receipt/v1";
pub(super) const DRIVER_SCHEMA: &str = "ai-platform/migration-n1-compatibility-driver/v1";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct DriverScenario {
    pub(super) id: String,
    pub(super) status: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct DriverResult {
    pub(super) schema: String,
    pub(super) status: String,
    pub(super) baseline_id: String,
    pub(super) successor_sequence: i32,
    pub(super) candidate_sql_sha256: String,
    pub(super) candidate_manifest_sha256: String,
    pub(super) compatibility_phase: String,
    pub(super) expected_database: String,
    pub(super) previous_release_id: String,
    pub(super) previous_application_sha256: String,
    pub(super) authority_applied_candidate: bool,
    pub(super) candidate_epoch_verified: bool,
    pub(super) previous_application_started: bool,
    pub(super) scenarios: Vec<DriverScenario>,
}

fn is_executable(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(refused("N-1 executable must be a regular non-symlink file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(refused("N-1 executable is not executable"));
        }
    }
    Ok(())
}

fn file_hash(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(refused("N-1 executable must be a regular non-symlink file"));
    }
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_release_id(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
}

fn output_destination(source_root: &Path, requested: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(requested)?;
    match fs::symlink_metadata(&absolute) {
        Ok(_) => return Err(refused("N-1 receipt output must be new")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let parent = absolute
        .parent()
        .ok_or_else(|| refused("N-1 receipt output parent is missing"))?
        .canonicalize()?;
    let resolved = parent.join(
        absolute
            .file_name()
            .ok_or_else(|| refused("N-1 receipt output name is missing"))?,
    );
    if absolute != resolved
        || absolute.starts_with(source_root)
        || source_root.starts_with(&absolute)
    {
        return Err(refused("N-1 receipt must be external to the source tree"));
    }
    Ok(absolute)
}

fn execute_driver(
    driver: &Path,
    source: &CandidateSource,
    options: &N1RunOptions,
    compatibility_phase: &str,
) -> Result<(Vec<u8>, i32, u128)> {
    let started = Instant::now();
    let mut child = Command::new(driver)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("AI_MIGRATION_N1_BASELINE_ID", &source.manifest.baseline_id)
        .env(
            "AI_MIGRATION_N1_SUCCESSOR_SEQUENCE",
            source.change.sequence.to_string(),
        )
        .env("AI_MIGRATION_N1_SQL_SHA256", &source.sql_sha256)
        .env("AI_MIGRATION_N1_MANIFEST_SHA256", &source.manifest_sha256)
        .env("AI_MIGRATION_N1_COMPATIBILITY_PHASE", compatibility_phase)
        .env(
            "AI_MIGRATION_N1_EXPECTED_DATABASE",
            &options.expected_database,
        )
        .env(
            "AI_MIGRATION_N1_PREVIOUS_RELEASE_ID",
            &options.previous_release_id,
        )
        .env(
            "AI_MIGRATION_N1_PREVIOUS_APPLICATION",
            &options.previous_application,
        )
        .env(
            "AI_MIGRATION_N1_PREVIOUS_APPLICATION_SHA256",
            &options.previous_application_sha256,
        )
        .env("AI_MIGRATION_N1_SOURCE_ROOT", &source.root)
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| refused("N-1 driver stdout could not be captured"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| refused("N-1 driver stderr could not be captured"))?;
    let stdout_reader = thread::spawn(move || read_limited(stdout));
    let stderr_reader = thread::spawn(move || read_limited(stderr));

    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= N1_DRIVER_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(refused("N-1 compatibility driver timed out"));
        }
        thread::sleep(Duration::from_millis(50));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| refused("N-1 driver stdout reader failed"))??;
    let _stderr = stderr_reader
        .join()
        .map_err(|_| refused("N-1 driver stderr reader failed"))??;
    let code = status.code().unwrap_or(-1);
    if !status.success() {
        return Err(refused("N-1 compatibility driver failed"));
    }
    Ok((stdout, code, started.elapsed().as_millis()))
}

fn read_limited<R: Read>(reader: R) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_DRIVER_STREAM_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_DRIVER_STREAM_BYTES {
        return Err(refused("N-1 driver output exceeded its bounded size"));
    }
    Ok(bytes)
}

pub(super) fn validate_driver_result(
    bytes: &[u8],
    source: &CandidateSource,
    options: &N1RunOptions,
    compatibility_phase: &str,
) -> Result<Vec<String>> {
    let result: DriverResult = serde_json::from_slice(bytes)
        .map_err(|_| refused("N-1 driver did not return its required JSON result"))?;
    if result.schema != DRIVER_SCHEMA
        || result.status != "passed"
        || result.baseline_id != source.manifest.baseline_id
        || result.successor_sequence != source.change.sequence
        || result.candidate_sql_sha256 != source.sql_sha256
        || result.candidate_manifest_sha256 != source.manifest_sha256
        || result.compatibility_phase != compatibility_phase
        || result.expected_database != options.expected_database
        || result.previous_release_id != options.previous_release_id
        || result.previous_application_sha256 != options.previous_application_sha256
        || !result.authority_applied_candidate
        || !result.candidate_epoch_verified
        || !result.previous_application_started
        || result.scenarios.is_empty()
    {
        return Err(refused(
            "N-1 driver result does not bind a completed candidate rehearsal",
        ));
    }
    let mut ids = BTreeSet::new();
    for scenario in &result.scenarios {
        if !valid_release_id(&scenario.id)
            || scenario.status != "passed"
            || !ids.insert(scenario.id.clone())
        {
            return Err(refused(
                "N-1 scenario set is incomplete or contains failures",
            ));
        }
    }
    Ok(ids.into_iter().collect())
}

pub fn rehearse_n1(source_root: &Path, sequence: i32, options: N1RunOptions) -> Result<N1Receipt> {
    let lint = lint_source(source_root, sequence)?;
    if lint.has_blockers() {
        return Err(refused(
            "N-1 rehearsal cannot bypass migration source-lint blockers",
        ));
    }
    if !valid_release_id(&options.previous_release_id)
        || !valid_sha256(&options.previous_application_sha256)
        || !valid_sha256(&options.driver_sha256)
    {
        return Err(refused(
            "N-1 release identity or executable digest is invalid",
        ));
    }
    ai_platform_migration_authority::source_tools::measure_successors::validate_database_name(
        &options.expected_database,
    )?;
    is_executable(&options.previous_application)?;
    is_executable(&options.driver)?;
    let previous_application_sha256 = file_hash(&options.previous_application)?;
    let driver_sha256 = file_hash(&options.driver)?;
    if previous_application_sha256 != options.previous_application_sha256
        || driver_sha256 != options.driver_sha256
    {
        return Err(refused("N-1 executable differs from its pinned SHA-256"));
    }
    let source = candidate_source(source_root, sequence)?;
    if source.sql_sha256 != lint.sql_sha256 || source.manifest_sha256 != lint.manifest_sha256 {
        return Err(refused(
            "migration source changed after the static lint completed",
        ));
    }
    let output = output_destination(&lint_source_root(source_root)?, &options.receipt_output)?;
    let (stdout, exit_code, elapsed_ms) =
        execute_driver(&options.driver, &source, &options, lint.compatibility_phase)?;
    let scenario_ids =
        validate_driver_result(&stdout, &source, &options, lint.compatibility_phase)?;
    let source_after_run = candidate_source(source_root, sequence)?;
    if source_after_run.sql_sha256 != source.sql_sha256
        || source_after_run.manifest_sha256 != source.manifest_sha256
    {
        return Err(refused(
            "migration source changed during N-1 compatibility rehearsal",
        ));
    }
    if file_hash(&options.previous_application)? != options.previous_application_sha256
        || file_hash(&options.driver)? != options.driver_sha256
    {
        return Err(refused(
            "N-1 executable changed during compatibility rehearsal",
        ));
    }
    let receipt = N1Receipt {
        schema: RECEIPT_SCHEMA,
        status: "n1_application_compatibility_executed_passed",
        baseline_id: source.manifest.baseline_id,
        successor_sequence: sequence,
        candidate_sql_sha256: source.sql_sha256,
        candidate_manifest_sha256: source.manifest_sha256,
        compatibility_phase: lint.compatibility_phase.to_owned(),
        expected_database: options.expected_database,
        previous_release_id: options.previous_release_id,
        previous_application_sha256,
        driver_sha256,
        driver_stdout_sha256: format!("{:x}", Sha256::digest(&stdout)),
        passed_scenarios: scenario_ids.len(),
        failed_scenarios: 0,
        scenario_ids,
        driver_exit_code: exit_code,
        elapsed_ms,
        executed_at_utc: chrono::Utc::now().to_rfc3339(),
    };
    write_new_json(&output, &receipt)?;
    Ok(receipt)
}

fn lint_source_root(path: &Path) -> Result<PathBuf> {
    canonical_source(path)
}

fn write_new_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|_| refused("N-1 receipt could not be serialized"))?;
    bytes.push(b'\n');
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(path);
        return Err(error.into());
    }
    Ok(())
}
