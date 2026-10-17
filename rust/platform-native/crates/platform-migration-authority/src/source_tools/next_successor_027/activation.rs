use super::*;

fn write_relative(root: &Path, relative: &str, bytes: &[u8]) -> Result<()> {
    let path = root.join(relative);
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| refused("027 invalid bundle path"))?,
    )?;
    safe::write_new_bytes(&path, bytes)
}
/// Builds an external review bundle only; canonical manifest, lock, and SQL stay unmounted.
pub fn activation_bundle(source: &Path, preview: &Path, output: &Path) -> Result<Value> {
    let source = safe::canonical_directory(source, "027 source must be a real directory")?;
    let preview = safe::canonical_directory(preview, "027 preview must be a real directory")?;
    let candidate = verify_preview(&source, &preview, true)?;
    let output = external_destination(&source, Some(&preview), output)?;
    let measurement = verify_measured_files(&source, &preview, &candidate)?;
    let manifest_sha = manifest::file_sha256(&preview.join(MANIFEST))?;
    let reference_sha = manifest::file_sha256(&epoch_file(&preview, REFERENCE))?;
    let measurement_sha = manifest::file_sha256(&epoch_file(&preview, MEASUREMENT))?;
    let database = measurement["expected_database"]
        .as_str()
        .ok_or_else(|| refused("027 measurement database is missing"))?;
    let activation = common_receipt(
        &candidate,
        &manifest_sha,
        "activated_source_chain",
        Some(database),
        Some(&reference_sha),
        Some(&measurement_sha),
    );
    let mut activation_bytes = serde_json::to_vec_pretty(&activation)
        .map_err(|_| refused("027 activation review serialization failed"))?;
    activation_bytes.push(b'\n');
    let mut package_lock = candidate.package_lock.clone();
    let files = package_lock["files_sha256"]
        .as_object_mut()
        .ok_or_else(|| refused("027 predecessor lock inventory is invalid"))?;
    for (name, hash) in [
        ("manifest.yml", manifest_sha.clone()),
        (SQL, SQL_SHA256.into()),
        (REFERENCE, reference_sha),
        (MEASUREMENT, measurement_sha),
        (ACTIVATION, safe::sha256_bytes(&activation_bytes)),
    ] {
        files.insert(epoch_relative(name), json!(hash));
    }
    let mut lock_bytes = serde_json::to_vec_pretty(&package_lock)
        .map_err(|_| refused("027 package lock serialization failed"))?;
    lock_bytes.push(b'\n');
    fs::create_dir(&output)?;
    let mut overlays = BTreeMap::new();
    for relative in [
        MANIFEST.to_owned(),
        format!("database/{}", epoch_relative(SQL)),
        format!("database/{}", epoch_relative(REFERENCE)),
        format!("database/{}", epoch_relative(MEASUREMENT)),
    ] {
        let bytes = fs::read(safe::source_file(&preview, &relative)?)?;
        write_relative(&output, &relative, &bytes)?;
        overlays.insert(relative, safe::sha256_bytes(&bytes));
    }
    for (relative, bytes) in [
        (
            format!("database/{}", epoch_relative(ACTIVATION)),
            activation_bytes,
        ),
        (LOCK.into(), lock_bytes),
        (
            ARCHIVED_LOCK.into(),
            fs::read(safe::source_file(&source, LOCK)?)?,
        ),
        (
            ARCHIVED_MANIFEST.into(),
            fs::read(safe::source_file(&source, MANIFEST)?)?,
        ),
    ] {
        write_relative(&output, &relative, &bytes)?;
        overlays.insert(relative, safe::sha256_bytes(&bytes));
    }
    for input in PROPOSAL_INPUTS {
        let bytes = fs::read(safe::source_file(&source, input.path)?)?;
        write_relative(&output, input.path, &bytes)?;
        overlays.insert(input.path.into(), safe::sha256_bytes(&bytes));
    }
    if validate_source(&source)?.plan_sha256 != candidate.plan_sha256 {
        return Err(refused("027 source changed while the bundle was prepared"));
    }
    let summary = json!({
        "schema": "ai-platform/canonical-fixed-successor-bundle/v1",
        "status": "ready_for_primary_activation",
        "database_actions": false,
        "source_activation": false,
        "predecessor_epoch": PREDECESSOR_EPOCH,
        "candidate_epoch": CANDIDATE_EPOCH,
        "source_plan_sha256": candidate.plan_sha256,
        "compiled_distribution_lock_sha256_to_review": overlays[LOCK],
        "files_sha256": overlays
    });
    safe::write_new_json(&output.join("ACTIVATION_027_REVIEW.json"), &summary)?;
    Ok(summary)
}

/// Rebuild the exact archived026 source view in owned external scratch, then run
/// its full unchanged verifier. No invented receipt or fingerprint is substituted.
struct PredecessorView(PathBuf);
impl Drop for PredecessorView {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn predecessor_view(root: &Path) -> Result<PredecessorView> {
    let parent = std::env::temp_dir().canonicalize()?;
    let directory = parent.join(format!(
        "agentforge-027-predecessor-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir(&directory)?;
    let view = PredecessorView(directory);
    safe::copy_tree(&root.join("database"), &view.0.join("database"))?;
    fs::create_dir(view.0.join("docs"))?;
    safe::copy_tree(&root.join("docs/design"), &view.0.join("docs/design"))?;
    fs::write(
        view.0.join(LOCK),
        fs::read(safe::source_file(root, ARCHIVED_LOCK)?)?,
    )?;
    fs::write(
        view.0.join(MANIFEST),
        fs::read(safe::source_file(root, ARCHIVED_MANIFEST)?)?,
    )?;
    for name in [SQL, REFERENCE, MEASUREMENT, ACTIVATION] {
        fs::remove_file(epoch_file(&view.0, name))?;
    }
    Ok(view)
}
pub fn verify_activated_source(root: &Path) -> Result<Value> {
    let root = safe::canonical_directory(root, "027 source must be a nonsymlink directory")?;
    // Current release must explicitly pin27 and accept the exact 026 predecessor;
    // this module never edits or bypasses those independently approved pins.
    distribution::verify(&root.join("database"), BASELINE)?;
    let epoch = manifest::load_epoch_manifest(&root.join(MANIFEST))?;
    if epoch.epoch != CANDIDATE_EPOCH {
        return Err(refused("027 canonical source is not activated"));
    }
    let view = predecessor_view(&root)?;
    let candidate = validate_source(&view.0)?;
    verify_inputs(&root, &candidate, true, true)?;
    let active_lock = source_json(&root, LOCK)?;
    let files = lock_files(&active_lock)?;
    if files.keys().cloned().collect::<BTreeSet<_>>() != inventory(&candidate, true, true)? {
        return Err(refused("027 activated lock coverage is invalid"));
    }
    // distribution::verify binds every current byte. This also checks the exact
    // unchanged026 predecessor before trusting new source receipts below.
    for (key, hash) in lock_files(&candidate.package_lock)? {
        if key != &epoch_relative("manifest.yml") && files.get(key) != Some(hash) {
            return Err(refused("027 activated lock changed predecessor terms"));
        }
    }
    let measurement = verify_measured_files(&root, &root, &candidate)?;
    let activation = source_json(&root, &format!("database/{}", epoch_relative(ACTIVATION)))?;
    let reference_sha = manifest::file_sha256(&epoch_file(&root, REFERENCE))?;
    let measurement_sha = manifest::file_sha256(&epoch_file(&root, MEASUREMENT))?;
    let database = measurement["expected_database"]
        .as_str()
        .ok_or_else(|| refused("027 measurement database is missing"))?;
    if activation
        != common_receipt(
            &candidate,
            &manifest::file_sha256(&root.join(MANIFEST))?,
            "activated_source_chain",
            Some(database),
            Some(&reference_sha),
            Some(&measurement_sha),
        )
    {
        return Err(refused(
            "027 activation does not bind the measured complete source chain",
        ));
    }
    Ok(activation)
}
