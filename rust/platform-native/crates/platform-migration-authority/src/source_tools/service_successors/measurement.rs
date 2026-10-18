use super::*;
fn reference_value(
    s: &Source,
    n: i32,
    e: &measure_successors::SingleStepEvidence,
) -> Result<Value> {
    let mut fingerprints = e.raw_fingerprints.clone();
    fingerprints.insert("acl".into(), e.base_acl_sha256.clone());
    let mut value = json!({"baseline_id":BASELINE,"sequence":n,"baseline_manifest_sha256":s.baseline_sha,
        "changes":e.applied.iter().map(|(n,h)|(n.to_string(),json!(h))).collect::<serde_json::Map<_,_>>(),
        "fingerprints":fingerprints,"raw_fingerprints":e.raw_fingerprints,
        "acl_compatibility":{"revision":crate::ledger_acl::REVISION,"manifest_sha256":crate::ledger_acl::MANIFEST_SHA256,"delta_line":"schema_acl:public:migrator:USAGE:grantopt=0","base_acl_sha256":e.base_acl_sha256,"effective_acl_sha256":e.raw_acl_sha256,"removed_delta_count":1,"raw_acl_lines":e.raw_acl_lines,"base_acl_lines":e.base_acl_lines},
        "provenance":"Actual native single-step isolated measurement; exact source-owned service schema, complete ordered ledger, exact ACL, unchanged extensions/reference data.",
        "provenance_details":{"catalog_sha256":scope_sha(n)?,"source_plan_sha256":digest(&plan(n)?)?,"candidate_sql_sha256":step(&s.catalog,n)?.change.sha256,"anchor_package_lock_sha256":s.catalog.anchor_files_sha256[LOCK],"predecessor_epoch":n-1,"git_baseline_provenance_verified":false}});
    if n >= 35 {
        let supplemental = e
            .partition_catalog
            .as_ref()
            .ok_or_else(|| refused("real supplemental partition catalog measurement required"))?;
        partition_catalog::verify_value(supplemental)?;
        value["supplemental_partition_catalog"] = supplemental.clone();
    }
    Ok(value)
}
/// Exact union, never a permission subset check.032 deliberately adds no ACL.
pub(super) fn exact_acl(previous: &Value, current: &Value, additions: &[String]) -> Result<()> {
    let lines = |v: &Value| -> Result<Vec<String>> {
        serde_json::from_value(v["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("raw ACL evidence malformed"))
    };
    let old = lines(previous)?;
    let new = lines(current)?;
    let mut expected = old.iter().cloned().collect::<BTreeSet<_>>();
    let old_count = expected.len();
    expected.extend(additions.iter().cloned());
    if old_count != old.len()
        || additions.iter().collect::<BTreeSet<_>>().len() != additions.len()
        || expected.len() != old_count + additions.len()
        || new.iter().cloned().collect::<BTreeSet<_>>() != expected
        || new.len() != expected.len()
    {
        return Err(refused(
            "raw ACL additions/removals/duplicates differ from exact minimal source policy",
        ));
    }
    for key in [
        "delta_line",
        "revision",
        "manifest_sha256",
        "removed_delta_count",
    ] {
        if previous["acl_compatibility"][key] != current["acl_compatibility"][key] {
            return Err(refused("ledger compatibility policy changed"));
        }
    }
    for key in ["extensions", "reference_data"] {
        if previous["raw_fingerprints"][key] != current["raw_fingerprints"][key]
            || previous["fingerprints"][key] != current["fingerprints"][key]
        {
            return Err(refused("nonstructural source contract changed"));
        }
    }
    if previous["raw_fingerprints"]["structural"] == current["raw_fingerprints"]["structural"] {
        return Err(refused("schema step produced no structural change"));
    }
    if additions.is_empty()
        != (previous["raw_fingerprints"]["acl"] == current["raw_fingerprints"]["acl"])
    {
        return Err(refused(
            "ACL fingerprint does not reflect the exact policy delta",
        ));
    }
    Ok(())
}
fn verify_reference(s: &Source, root: &Path, n: i32, value: &Value) -> Result<()> {
    let raw: BTreeMap<String, String> = serde_json::from_value(value["raw_fingerprints"].clone())
        .map_err(|_| refused("raw fingerprint classes invalid"))?;
    if raw.keys().ne(s.baseline.fingerprints().keys())
        || raw.values().any(|h| {
            h.len() != 64
                || !h
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return Err(refused("all actual raw fingerprint classes required"));
    }
    let lines: Vec<String> =
        serde_json::from_value(value["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("raw ACL lines absent"))?;
    let (base_acl_sha256, raw_acl_sha256, base_acl_lines) =
        measure_successors::acl_reference(&raw, &lines)?;
    let manifest = manifest_value(s, n)?;
    let applied = manifest["changes"]
        .as_array()
        .ok_or_else(|| refused("expected change prefix invalid"))?
        .iter()
        .map(|c| {
            Ok((
                c["sequence"]
                    .as_i64()
                    .ok_or_else(|| refused("expected sequence invalid"))? as i32,
                c["sha256"]
                    .as_str()
                    .ok_or_else(|| refused("expected checksum invalid"))?
                    .to_owned(),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let evidence = measure_successors::SingleStepEvidence {
        applied,
        raw_fingerprints: raw,
        base_acl_sha256,
        raw_acl_sha256,
        raw_acl_lines: lines,
        base_acl_lines,
        partition_catalog: Some(value["supplemental_partition_catalog"].clone())
            .filter(|_| n >= 35),
    };
    if value != &reference_value(s, n, &evidence)? {
        return Err(refused(
            "measurement reference is not the exact source-bound full prefix",
        ));
    }
    let previous = read_json(root, &format!("database/{}", ep(&reference_name(n - 1))))?;
    exact_acl(&previous, value, &step(&s.catalog, n)?.acl_additions)
}
fn previous_receipt(s: &Source, root: &Path, n: i32) -> Result<String> {
    if n == 30 {
        Ok(s.catalog.anchor_files_sha256
            [&format!("database/{}", ep("SUCCESSOR_029_MEASUREMENT.json"))]
            .clone())
    } else {
        manifest::file_sha256(&safe::source_file(
            root,
            &format!("database/{}", ep(&receipt_name(n - 1))),
        )?)
    }
}
fn receipt(s: &Source, root: &Path, n: i32, database: &str) -> Result<Value> {
    Ok(
        json!({"schema":"ai-platform/source-owned-service-measurement/v1","status":"measured_external_preview","anchor_epoch":29,"predecessor_epoch":n-1,"candidate_epoch":n,"catalog_sha256":scope_sha(n)?,"source_plan_sha256":digest(&plan(n)?)?,"candidate_manifest_sha256":manifest_sha(s,n)?,"candidate_sql_sha256":step(&s.catalog,n)?.change.sha256,"verification_ref_sha256":manifest::file_sha256(&file(root,&reference_name(n)))?,"predecessor_measurement_sha256":previous_receipt(s,root,n)?,"expected_database":database,"post_migration_noop":true,"source_activation":false,"database_actions":true,"batch_submission_activated":false}),
    )
}
pub(super) fn verify_chain(
    s: &Source,
    root: &Path,
    through: i32,
    current_measured: bool,
) -> Result<()> {
    for n in 30..=through {
        if n == through && !current_measured {
            continue;
        }
        let value = read_json(root, &format!("database/{}", ep(&reference_name(n))))?;
        verify_reference(s, root, n, &value)?;
        let actual = read_json(root, &format!("database/{}", ep(&receipt_name(n))))?;
        let db = actual["expected_database"]
            .as_str()
            .ok_or_else(|| refused("measurement database missing"))?;
        measure_successors::validate_database_name(db)?;
        if actual != receipt(s, root, n, db)? {
            return Err(refused(
                "measurement receipt changed or does not bind the actual previous step",
            ));
        }
    }
    Ok(())
}
#[derive(Clone, Debug)]
pub struct MeasureOptions {
    pub source_root: PathBuf,
    pub preview_root: PathBuf,
    pub sequence: i32,
    pub expected_database: String,
    pub run_approved: bool,
}
pub async fn measure(o: MeasureOptions) -> Result<()> {
    if !o.run_approved {
        return Err(refused("explicit isolated measurement approval required"));
    }
    measure_successors::validate_database_name(&o.expected_database)?;
    verify_preview(&o.source_root, &o.preview_root, o.sequence, false)?;
    let s = validate(&o.source_root, o.sequence)?;
    let preview = safe::canonical_directory(&o.preview_root, "preview must be real")?;
    let epoch = manifest::load_epoch_manifest(&preview.join(MANIFEST))?;
    measure_successors::measure_single_step(
        &preview.join("database"),
        &o.expected_database,
        o.sequence - 1,
        o.sequence,
        &s.baseline,
        &s.baseline_sha,
        &epoch,
        true,
        |e| {
            let reference = reference_value(&s, o.sequence, e)?;
            verify_reference(&s, &preview, o.sequence, &reference)?;
            safe::write_reference(&file(&preview, &reference_name(o.sequence)), &reference)
        },
    )
    .await?;
    validate(&o.source_root, o.sequence)?;
    safe::write_reference(
        &file(&preview, &receipt_name(o.sequence)),
        &receipt(&s, &preview, o.sequence, &o.expected_database)?,
    )?;
    verify_preview(&o.source_root, &preview, o.sequence, true)?;
    Ok(())
}
