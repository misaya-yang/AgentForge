use super::*;
pub(super) fn activation_value(s: &Source, root: &Path, n: i32) -> Result<Value> {
    let mut measured = BTreeMap::new();
    for k in 30..=n {
        for name in [reference_name(k), receipt_name(k)] {
            measured.insert(name.clone(), manifest::file_sha256(&file(root, &name))?);
        }
    }
    Ok(
        json!({"schema":"ai-platform/source-owned-service-activation/v1","status":"activated_source_owned_chain","anchor_epoch":29,"predecessor_epoch":29,"candidate_epoch":n,"catalog_sha256":scope_sha(n)?,"source_plan_sha256":digest(&plan(n)?)?,"anchor_package_lock_sha256":s.catalog.anchor_files_sha256[LOCK],"candidate_manifest_sha256":manifest_sha(s,n)?,"measured_files_sha256":measured,"database_actions":false,"source_activation":false,"batch_submission_activated":false,"release_eligible":false,"n1_compatibility":"not_run"}),
    )
}
fn lock_value(s: &Source, root: &Path, n: i32, _activation: &Value) -> Result<Value> {
    let mut lock = s.base_lock.clone();
    let files = lock["files_sha256"]
        .as_object_mut()
        .ok_or_else(|| refused("anchor lock invalid"))?;
    files.insert(ep("manifest.yml"), json!(manifest_sha(s, n)?));
    for k in 30..=n {
        let spec = step(&s.catalog, k)?;
        for name in [spec.change.file.clone(), reference_name(k), receipt_name(k)] {
            files.insert(ep(&name), json!(manifest::file_sha256(&file(root, &name))?));
        }
    }
    for k in 30..=n {
        files.insert(
            ep(&activation_name(k)),
            json!(safe::sha256_bytes(&bytes(&activation_value(s, root, k)?)?)),
        );
    }
    Ok(lock)
}
fn write_overlay(
    output: &Path,
    name: &str,
    data: &[u8],
    files: &mut BTreeMap<String, String>,
) -> Result<()> {
    let path = output.join(name);
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| refused("overlay parent invalid"))?,
    )?;
    safe::write_new_bytes(&path, data)?;
    files.insert(name.into(), safe::sha256_bytes(data));
    Ok(())
}
pub fn activation_bundle(source: &Path, preview: &Path, output: &Path, n: i32) -> Result<Value> {
    verify_preview(source, preview, n, true)?;
    let s = validate(source, n)?;
    let preview = safe::canonical_directory(preview, "preview must be real")?;
    let output = destination(&s.input_root, Some(&preview), output)?;
    let activation = activation_value(&s, &preview, n)?;
    let lock = lock_value(&s, &preview, n, &activation)?;
    fs::create_dir(&output)?;
    let mut overlays = BTreeMap::new();
    write_overlay(
        &output,
        MANIFEST,
        &fs::read(preview.join(MANIFEST))?,
        &mut overlays,
    )?;
    write_overlay(&output, LOCK, &bytes(&lock)?, &mut overlays)?;
    write_overlay(
        &output,
        ARCHIVED_LOCK,
        &fs::read(s.root.join(LOCK))?,
        &mut overlays,
    )?;
    write_overlay(
        &output,
        ARCHIVED_MANIFEST,
        &fs::read(s.root.join(MANIFEST))?,
        &mut overlays,
    )?;
    for k in 30..=n {
        write_overlay(
            &output,
            &format!("database/{}", ep(&activation_name(k))),
            &bytes(&activation_value(&s, &preview, k)?)?,
            &mut overlays,
        )?;
    }
    for k in 30..=n {
        let spec = step(&s.catalog, k)?;
        for name in [spec.change.file.clone(), reference_name(k), receipt_name(k)] {
            write_overlay(
                &output,
                &format!("database/{}", ep(&name)),
                &fs::read(file(&preview, &name))?,
                &mut overlays,
            )?;
        }
    }
    for name in [
        "schema_admission_expected.json",
        "service_admission_requirements.json",
    ] {
        write_overlay(
            &output,
            &format!("docs/design/service-successors-{name}"),
            &fs::read(preview.join(name))?,
            &mut overlays,
        )?;
    }
    if n >= 35 {
        for (source, destination) in [
            (
                "docs/design/proposals/data-platform/append-partitions/acceptance-catalog.sql",
                "docs/design/service-successors-partition-acceptance.sql",
            ),
            (
                "docs/design/proposals/service-successors/partition-authority-policy.json",
                "docs/design/service-successors-partition-authority-policy.json",
            ),
        ] {
            write_overlay(
                &output,
                destination,
                &fs::read(safe::source_file(&s.root, source)?)?,
                &mut overlays,
            )?;
        }
    }
    for path in [
        "rust/agent-runtime-overlay/kernel-rs/ai-platform-agent-runtime/src/schema_admission_expected.json",
        "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/schema_admission_expected.json",
    ] {
        write_overlay(
            &output,
            path,
            &bytes(&admission_snapshot(&s, n)?)?,
            &mut overlays,
        )?;
    }
    let result = json!({"schema":"ai-platform/source-owned-service-bundle/v1","status":"ready_for_primary_activation_review","candidate_epoch":n,"anchor_epoch":29,"catalog_sha256":scope_sha(n)?,"source_plan_sha256":digest(&plan(n)?)?,"compiled_distribution_lock_sha256_to_review":overlays[LOCK],"files_sha256":overlays,"database_actions":false,"source_activation":false,"n1_compatibility":"not_run","release_eligible":false});
    safe::write_new_json(
        &output.join("SERVICE_SUCCESSOR_ACTIVATION_REVIEW.json"),
        &result,
    )?;
    Ok(result)
}
pub(super) struct Anchor(pub(super) PathBuf);
impl Drop for Anchor {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
pub(super) fn restore_anchor(root: &Path, n: i32) -> Result<Anchor> {
    let path = std::env::temp_dir()
        .canonicalize()?
        .join(format!("service-successor-anchor-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path)?;
    let anchor = Anchor(path);
    safe::copy_tree(&root.join("database"), &anchor.0.join("database"))?;
    fs::create_dir(anchor.0.join("docs"))?;
    safe::copy_tree(&root.join("docs/design"), &anchor.0.join("docs/design"))?;
    copy_selected_inputs(root, &anchor.0, n)?;
    fs::write(
        anchor.0.join(LOCK),
        fs::read(safe::source_file(root, ARCHIVED_LOCK)?)?,
    )?;
    fs::write(
        anchor.0.join(MANIFEST),
        fs::read(safe::source_file(root, ARCHIVED_MANIFEST)?)?,
    )?;
    for k in 30..=n {
        let c = catalog()?;
        for name in [
            step(&c, k)?.change.file.clone(),
            reference_name(k),
            receipt_name(k),
        ] {
            fs::remove_file(file(&anchor.0, &name))?;
        }
    }
    for k in 30..=n {
        fs::remove_file(file(&anchor.0, &activation_name(k)))?;
    }
    Ok(anchor)
}
pub fn verify_activated_source(root: &Path) -> Result<Value> {
    let root = safe::canonical_directory(root, "active service source must be real")?;
    let epoch = manifest::load_epoch_manifest(&root.join(MANIFEST))?;
    step(&catalog()?, epoch.epoch)?;
    // The primary must compile the reviewed new distribution pin at activation.
    distribution::verify(&root.join("database"), BASELINE)?;
    let anchor = restore_anchor(&root, epoch.epoch)?;
    let s = validate(&anchor.0, epoch.epoch)?;
    let n = epoch.epoch;
    if fs::read(root.join(ARCHIVED_MANIFEST))? != fs::read(s.root.join(MANIFEST))?
        || fs::read(root.join(ARCHIVED_LOCK))? != fs::read(s.root.join(LOCK))?
    {
        return Err(refused("actual029 predecessor archives changed"));
    }
    let mut expected = inventory(&s, n, true)?;
    for k in 30..=n {
        expected.insert(ep(&activation_name(k)));
    }
    if safe::collect_distribution_inputs(&root.join("database"))? != expected {
        return Err(refused("active package coverage differs"));
    }
    for (relative, hash) in s.base_lock["files_sha256"]
        .as_object()
        .ok_or_else(|| refused("anchor inventory invalid"))?
    {
        if relative != &ep("manifest.yml")
            && manifest::file_sha256(&safe::source_file(&root, &format!("database/{relative}"))?)?
                != hash.as_str().unwrap_or("")
        {
            return Err(refused("active source changed a frozen029 input"));
        }
    }
    if fs::read(root.join(MANIFEST))? != bytes(&manifest_value(&s, n)?)? {
        return Err(refused(
            "active manifest is not the fixed catalog extension",
        ));
    }
    for k in 30..=n {
        let spec = step(&s.catalog, k)?;
        if fs::read(file(&root, &spec.change.file))? != render(&s.root, &s.catalog, spec)? {
            return Err(refused("active SQL changed"));
        }
    }
    measurement::verify_chain(&s, &root, n, true)?;
    let activation = activation_value(&s, &root, n)?;
    if read_json(&root, &format!("database/{}", ep(&activation_name(n))))? != activation
        || fs::read(root.join(LOCK))? != bytes(&lock_value(&s, &root, n, &activation)?)?
    {
        return Err(refused(
            "active receipt/distribution does not bind the full measured chain",
        ));
    }
    Ok(activation)
}

/// Read-only historical029 witness, after verifying any actual activated service prefix.
pub fn verify_anchor_029(root: &Path) -> Result<Value> {
    let epoch = manifest::load_epoch_manifest(&root.join(MANIFEST))?.epoch;
    if epoch == 29 {
        return next_successor_029::verify_activated_source(root);
    }
    verify_activated_source(root)?;
    let anchor = restore_anchor(root, epoch)?;
    next_successor_029::verify_activated_source(&anchor.0)
}
