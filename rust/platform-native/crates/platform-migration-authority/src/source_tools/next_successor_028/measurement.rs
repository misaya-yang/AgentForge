use super::*;

fn reference_value(
    candidate: &Candidate,
    evidence: &measure_successors::SingleStepEvidence,
) -> Value {
    let mut fingerprints = evidence.raw_fingerprints.clone();
    fingerprints.insert("acl".into(), evidence.base_acl_sha256.clone());
    let changes = evidence
        .applied
        .iter()
        .map(|(sequence, checksum)| (sequence.to_string(), json!(checksum)))
        .collect::<serde_json::Map<_, _>>();
    json!({
        "baseline_id": BASELINE,
        "sequence": CANDIDATE_EPOCH,
        "baseline_manifest_sha256": candidate.baseline_sha256,
        "changes": changes,
        "fingerprints": fingerprints,
        "raw_fingerprints": evidence.raw_fingerprints,
        "acl_compatibility": {
            "revision": crate::ledger_acl::REVISION,
            "manifest_sha256": crate::ledger_acl::MANIFEST_SHA256,
            "delta_line": ACL_DELTA,
            "base_acl_sha256": evidence.base_acl_sha256,
            "effective_acl_sha256": evidence.raw_acl_sha256,
            "removed_delta_count": 1,
            "raw_acl_lines": evidence.raw_acl_lines,
            "base_acl_lines": evidence.base_acl_lines
        },
        "provenance": PROVENANCE,
        "provenance_details": {
            "tool": TOOL,
            "predecessor_epoch": PREDECESSOR_EPOCH,
            "approved_predecessor_package_lock_sha256": candidate.package_lock_sha256,
            "predecessor_manifest_sha256": candidate.plan["predecessor_manifest_sha256"],
            "predecessor_reference_sha256": candidate.predecessor_reference_sha256,
            "predecessor_measurement_receipt_sha256": candidate.predecessor_measurement_sha256,
            "predecessor_activation_receipt_sha256": candidate.predecessor_activation_sha256,
            "source_plan_sha256": candidate.plan_sha256,
            "candidate_sql_sha256": SQL_SHA256,
            "git_baseline_provenance_verified": false
        }
    })
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn verify_unchanged_acl_and_metadata(previous: &Value, current: &Value) -> Result<()> {
    let old_lines: Vec<String> =
        serde_json::from_value(previous["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("028 predecessor ACL lines are malformed"))?;
    let new_lines: Vec<String> =
        serde_json::from_value(current["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("028 measured ACL lines are malformed"))?;
    if old_lines != new_lines {
        return Err(refused("028 changed the measured ACL lines"));
    }
    for key in [
        "revision",
        "manifest_sha256",
        "delta_line",
        "removed_delta_count",
        "base_acl_sha256",
        "effective_acl_sha256",
        "base_acl_lines",
    ] {
        if previous["acl_compatibility"][key] != current["acl_compatibility"][key] {
            return Err(refused("028 changed ACL compatibility evidence"));
        }
    }
    for key in ["acl", "extensions", "reference_data"] {
        if previous["raw_fingerprints"][key] != current["raw_fingerprints"][key]
            || previous["fingerprints"][key] != current["fingerprints"][key]
        {
            return Err(refused(
                "028 changed ACL, extension, or reference-data fingerprints",
            ));
        }
    }
    if previous["raw_fingerprints"]["structural"] == current["raw_fingerprints"]["structural"] {
        return Err(refused(
            "028 measurement did not change the structural fingerprint",
        ));
    }
    Ok(())
}

fn verify_reference_value(source: &Path, candidate: &Candidate, reference: &Value) -> Result<()> {
    let raw: BTreeMap<String, String> =
        serde_json::from_value(reference["raw_fingerprints"].clone())
            .map_err(|_| refused("028 raw fingerprints are malformed"))?;
    if raw.keys().ne(candidate.baseline.fingerprints().keys())
        || raw.values().any(|hash| !valid_hash(hash))
    {
        return Err(refused("028 requires all valid raw fingerprint classes"));
    }
    let lines: Vec<String> =
        serde_json::from_value(reference["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("028 raw ACL evidence is missing"))?;
    let (base_acl_sha256, raw_acl_sha256, base_acl_lines) =
        measure_successors::acl_reference(&raw, &lines)?;
    let applied = candidate
        .epoch
        .changes
        .iter()
        .map(|change| (change.sequence, change.sha256.clone()))
        .collect();
    let evidence = measure_successors::SingleStepEvidence {
        applied,
        raw_fingerprints: raw,
        base_acl_sha256,
        raw_acl_sha256,
        raw_acl_lines: lines,
        base_acl_lines,
    };
    if reference != &reference_value(candidate, &evidence) {
        return Err(refused(
            "028 reference does not bind the complete prefix and fixed source",
        ));
    }
    let previous_path = safe::source_file(
        source,
        &format!("database/{}", epoch_relative("027_verification.json")),
    )?;
    if lock_files(&candidate.package_lock)?.get(&epoch_relative("027_verification.json"))
        != Some(&json!(manifest::file_sha256(&previous_path)?))
    {
        return Err(refused(
            "028 predecessor reference differs from the activated 027 lock",
        ));
    }
    let previous: Value = serde_json::from_slice(&fs::read(previous_path)?)
        .map_err(|_| refused("028 predecessor reference is malformed"))?;
    verify_unchanged_acl_and_metadata(&previous, reference)?;
    Ok(())
}

pub(super) fn verify_measured_files(
    source: &Path,
    preview: &Path,
    candidate: &Candidate,
) -> Result<Value> {
    let reference = source_json(preview, &format!("database/{}", epoch_relative(REFERENCE)))?;
    verify_reference_value(source, candidate, &reference)?;
    let receipt = source_json(
        preview,
        &format!("database/{}", epoch_relative(MEASUREMENT)),
    )?;
    let database = receipt["expected_database"]
        .as_str()
        .ok_or_else(|| refused("028 measurement lacks its isolated database name"))?;
    measure_successors::validate_database_name(database)?;
    let reference_sha = manifest::file_sha256(&epoch_file(preview, REFERENCE))?;
    if receipt
        != common_receipt(
            candidate,
            &manifest::file_sha256(&preview.join(MANIFEST))?,
            "measured_external_preview",
            Some(database),
            Some(&reference_sha),
            None,
        )
    {
        return Err(refused(
            "028 measurement receipt is not bound to this preview",
        ));
    }
    Ok(receipt)
}

#[derive(Clone, Debug)]
pub struct MeasureOptions {
    pub source_root: PathBuf,
    pub preview_root: PathBuf,
    pub expected_database: String,
    pub run_approved: bool,
}

pub async fn measure(options: MeasureOptions) -> Result<()> {
    if !options.run_approved {
        return Err(refused(
            "028 measurement requires explicit isolated-database approval",
        ));
    }
    measure_successors::validate_database_name(&options.expected_database)?;
    let candidate = verify_preview(&options.source_root, &options.preview_root, false)?;
    let source =
        safe::canonical_directory(&options.source_root, "028 source must be a real directory")?;
    let preview = safe::canonical_directory(
        &options.preview_root,
        "028 preview must be a real directory",
    )?;
    measure_successors::measure_single_step(
        &preview.join("database"),
        &options.expected_database,
        PREDECESSOR_EPOCH,
        CANDIDATE_EPOCH,
        &candidate.baseline,
        &candidate.baseline_sha256,
        &candidate.epoch,
        options.run_approved,
        |evidence| {
            let reference = reference_value(&candidate, evidence);
            verify_reference_value(&source, &candidate, &reference)?;
            safe::write_reference(&epoch_file(&preview, REFERENCE), &reference)
        },
    )
    .await?;
    let rechecked = validate_source(&source)?;
    if rechecked.plan_sha256 != candidate.plan_sha256 {
        return Err(refused("028 source changed during isolated measurement"));
    }
    let reference_sha = manifest::file_sha256(&epoch_file(&preview, REFERENCE))?;
    let receipt = common_receipt(
        &candidate,
        &manifest::file_sha256(&preview.join(MANIFEST))?,
        "measured_external_preview",
        Some(&options.expected_database),
        Some(&reference_sha),
        None,
    );
    safe::write_reference(&epoch_file(&preview, MEASUREMENT), &receipt)?;
    verify_preview(&source, &preview, true)?;
    Ok(())
}
