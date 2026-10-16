//! Isolated 022 -> 023 measurement through the shared canonical one-step runner.
use crate::{
    AuthorityError, Result, manifest,
    source_tools::{measure_successors, next_successor, next_successor_023},
};
use serde_json::json;
use std::path::{Path, PathBuf};

const MEASUREMENT_FILE: &str = "SUCCESSOR_023_MEASUREMENT.json";
const REFERENCE_FILE: &str = "023_verification.json";

fn refused(message: &str) -> AuthorityError {
    AuthorityError::Refused(message.into())
}

#[derive(Debug, Clone)]
pub struct MeasureOptions {
    pub source_root: PathBuf,
    pub preview_root: PathBuf,
    pub expected_database: String,
    pub run_approved: bool,
}

fn write_measurement_receipt(
    preview_root: &Path,
    candidate: &next_successor_023::Candidate,
    expected_database: &str,
) -> Result<()> {
    let epoch_dir = preview_root.join("database").join("migrations").join(next_successor_023::BASELINE);
    let reference_path = epoch_dir.join(REFERENCE_FILE);
    let manifest_path = epoch_dir.join("manifest.yml");
    let receipt = json!({
        "schema":"ai-platform/canonical-next-successor-measurement/v1",
        "status":"measured_external_preview",
        "database_actions":true,
        "database_actions_scope":"named_isolated_database_only",
        "source_activation":false,
        "expected_database":expected_database,
        "predecessor_epoch":next_successor_023::PREDECESSOR_EPOCH,
        "candidate_epoch":next_successor_023::CANDIDATE_EPOCH,
        "source_plan_sha256":candidate.plan_sha256,
        "approved_predecessor_package_lock_sha256":candidate.package_lock_sha256,
        "predecessor_manifest_sha256":candidate.plan["predecessor_manifest_sha256"],
        "candidate_manifest_sha256":manifest::file_sha256(&manifest_path)?,
        "baseline_manifest_sha256":candidate.baseline_sha256,
        "candidate_sql_sha256":candidate.proposal_sha256,
        "successor_sequences":[next_successor_023::CANDIDATE_EPOCH],
        "predecessor_measurement_receipt_sha256":candidate.predecessor_measurement_sha256,
        "predecessor_activation_receipt_sha256":candidate.predecessor_activation_sha256,
        "verification_ref_sha256":manifest::file_sha256(&reference_path)?,
        "post_migration_noop":true
    });
    next_successor::write_reference(&epoch_dir.join(MEASUREMENT_FILE), &receipt)
}

async fn measure(options: MeasureOptions) -> Result<()> {
    let (candidate, _) = next_successor_023::verify_preview(&options.source_root, &options.preview_root)?;
    let source_root = options.source_root.canonicalize()?;
    let preview_root = options.preview_root.canonicalize()?;
    let epoch_dir = preview_root.join("database").join("migrations").join(next_successor_023::BASELINE);
    measure_successors::measure_single_step(
        &preview_root.join("database"),
        &options.expected_database,
        next_successor_023::PREDECESSOR_EPOCH,
        next_successor_023::CANDIDATE_EPOCH,
        &candidate.baseline,
        &candidate.baseline_sha256,
        &candidate.epoch,
        options.run_approved,
        |evidence| {
            let reference = next_successor_023::verification_reference(&candidate, evidence);
            next_successor::write_reference(&epoch_dir.join(REFERENCE_FILE), &reference)
        },
    )
    .await?;
    let rechecked = next_successor_023::validate_source(&source_root)?;
    if rechecked.plan_sha256 != candidate.plan_sha256
        || rechecked.package_lock_sha256 != candidate.package_lock_sha256
        || rechecked.proposal_sha256 != candidate.proposal_sha256
    {
        return Err(refused("023 candidate source changed during measurement"));
    }
    write_measurement_receipt(&preview_root, &candidate, &options.expected_database)?;
    next_successor_023::verify_measured_preview(&source_root, &preview_root)?;
    Ok(())
}

pub async fn run(options: MeasureOptions) -> Result<()> {
    measure(options).await
}
