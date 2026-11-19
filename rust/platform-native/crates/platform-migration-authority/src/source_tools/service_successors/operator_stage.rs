//! Offline bridge from the existing native append operation planner to source035's
//! owner-only approval gate. Reads and hashes real artifacts; never executes SQL,
//! connects a database, or promotes artifact hashes to acceptance evidence.
use super::*;
use chrono::{DateTime, Utc};
use std::io::Read;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceFile {
    path: PathBuf,
    sha256: String,
}
fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let m = fs::symlink_metadata(path)?;
    if !path.is_absolute()
        || !m.is_file()
        || m.file_type().is_symlink()
        || m.len() == 0
        || m.len() > limit
    {
        return Err(refused(
            "bounded absolute regular operator artifact required",
        ));
    }
    let mut data = Vec::new();
    fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut data)?;
    if data.is_empty() || data.len() as u64 > limit {
        return Err(refused("operator artifact size changed"));
    }
    Ok(data)
}
fn sha(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn review(plan: &Value, files: &BTreeMap<String, EvidenceFile>) -> Result<Value> {
    let expected = [
        "authority_snapshot",
        "pitr_restore_drill",
        "role_rls_acceptance",
        "n_minus_one_compatibility",
        "dependency_rebind",
        "authority_approval",
        "native_writer_acceptance",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    if files.keys().cloned().collect::<BTreeSet<_>>() != expected {
        return Err(refused("all seven exact operator artifact kinds required"));
    }
    let policy:Value=serde_json::from_slice(include_bytes!("../../../../../../../docs/design/proposals/service-successors/partition-authority-policy.json")).map_err(|_|refused("partition operator policy invalid"))?;
    let family = plan["family"]
        .as_str()
        .ok_or_else(|| refused("operator family missing"))?;
    let stage = plan["operation"]["operation"]
        .as_str()
        .ok_or_else(|| refused("operator stage missing"))?;
    let revision = plan["expected_revision"]
        .as_i64()
        .filter(|n| *n >= 0)
        .ok_or_else(|| refused("operator revision missing"))?;
    if plan["schema"] != "agentforge-append-operation-plan/v1"
        || !policy["families"]
            .as_array()
            .is_some_and(|all| all.iter().any(|v| v == family))
        || !matches!(stage, "expand" | "cutover")
        || plan["effects"] != false
        || plan["ga_acceptance"] != false
        || plan["nontransactional_steps_present"] != false
        || plan["execution_owner"] != "native-migration-authority"
        || plan["requires_fresh_owner_snapshot_after_each_step"] != true
        || plan["required_connection_settings"]
            != json!({"lock_timeout":"2s","statement_timeout":"60s","TimeZone":"UTC"})
    {
        return Err(refused(
            "only exact non-executed native expand/cutover review plans accepted",
        ));
    }
    let mut checked = BTreeMap::new();
    for (kind, f) in files {
        if !sha(&f.sha256) || safe::sha256_bytes(&bounded(&f.path, 16 * 1024 * 1024)?) != f.sha256 {
            return Err(refused("operator evidence artifact digest mismatch"));
        }
        checked.insert(kind.clone(), f.sha256.clone());
    }
    if plan["authority_snapshot_sha256"] != checked["authority_snapshot"] {
        return Err(refused(
            "operator snapshot artifact differs from native plan",
        ));
    }
    let scope = if stage == "expand" {
        checked["authority_snapshot"].clone()
    } else {
        let five = checked
            .iter()
            .filter(|(kind, _)| {
                !matches!(
                    kind.as_str(),
                    "authority_snapshot" | "native_writer_acceptance"
                )
            })
            .collect::<BTreeMap<_, _>>();
        safe::sha256_bytes(
            &serde_json::to_vec(&five)
                .map_err(|_| refused("operator artifact map encoding failed"))?,
        )
    };
    let expected_sql = if stage == "expand" {
        let at = plan["operation"]["boundary"]
            .as_str()
            .ok_or_else(|| refused("operator boundary missing"))?
            .parse::<DateTime<Utc>>()
            .map_err(|_| refused("operator boundary invalid"))?;
        format!(
            "SELECT partition_ops.expand('{family}','{}'::timestamptz,'{scope}')",
            at.to_rfc3339()
        )
    } else {
        format!("SELECT partition_ops.cutover('{family}',{revision},'{scope}')")
    };
    if plan["sql"] != json!([expected_sql]) {
        return Err(refused(
            "operator plan SQL differs from fixed operation/snapshot/artifact scope",
        ));
    }
    let mut evidence = json!({"family":family,"stage":stage,"operation_scope_sha256":scope,"native_operation_plan_sha256":digest(plan)?,"source_catalog_scope_sha256":scope_sha(35)?});
    if stage == "expand" {
        evidence["operation_boundary"] = plan["operation"]["boundary"].clone();
    }
    for (kind, hash) in &checked {
        evidence[format!("{kind}_sha256")] = json!(hash);
    }
    Ok(
        json!({"schema":"agentforge/partition-operator-stage-review/v1","status":"artifact_bytes_checked_operator_review_required","candidate_source_epoch":35,"family":family,"stage":stage,"expected_revision":revision,"scope_sha256":scope,"approval_statement":"SELECT partition_ops.approve_operator_stage($1,$2,$3,$4,$5)","approval_parameters":[family,stage,revision,scope,evidence],"operator_authorization_required":true,"artifact_content_acceptance":"not_established_by_hash_validation","requires_installed_source_and_fresh_owner_snapshot":true,"effects":false,"database_actions":false,"ga_acceptance":false,"release_eligible":false}),
    )
}
pub fn review_files(source: &Path, plan_path: &Path, evidence_path: &Path) -> Result<Value> {
    validate_source(source, 35)?;
    let plan: Value = serde_json::from_slice(&bounded(plan_path, 4 * 1024 * 1024)?)
        .map_err(|_| refused("operator plan JSON invalid"))?;
    let files: BTreeMap<String, EvidenceFile> =
        serde_json::from_slice(&bounded(evidence_path, 1024 * 1024)?)
            .map_err(|_| refused("operator artifact manifest invalid"))?;
    review(&plan, &files)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn staged_operator_bridge_binds_actual_bytes_and_never_claims_acceptance() {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("partition-review-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _scratch = Scratch(root.clone());
        let mut files = BTreeMap::new();
        for name in [
            "authority_snapshot",
            "pitr_restore_drill",
            "role_rls_acceptance",
            "n_minus_one_compatibility",
            "dependency_rebind",
            "authority_approval",
            "native_writer_acceptance",
        ] {
            let path = root.join(name);
            let data = format!("synthetic unit artifact {name}");
            fs::write(&path, &data).unwrap();
            files.insert(
                name.into(),
                EvidenceFile {
                    path,
                    sha256: safe::sha256_bytes(data.as_bytes()),
                },
            );
        }
        let h = files["authority_snapshot"].sha256.clone();
        let plan = json!({"schema":"agentforge-append-operation-plan/v1","family":"gateway.audit_logs","operation":{"operation":"expand","boundary":"2030-01-01T00:00:00Z"},"expected_revision":0,"authority_snapshot_sha256":h,"effects":false,"ga_acceptance":false,"nontransactional_steps_present":false,"execution_owner":"native-migration-authority","requires_fresh_owner_snapshot_after_each_step":true,"required_connection_settings":{"lock_timeout":"2s","statement_timeout":"60s","TimeZone":"UTC"},"sql":[format!("SELECT partition_ops.expand('gateway.audit_logs','2030-01-01T00:00:00+00:00'::timestamptz,'{h}')")]});
        let output = review(&plan, &files).unwrap();
        assert_eq!(output["ga_acceptance"], false);
        assert_eq!(output["database_actions"], false);
        assert_eq!(output["operator_authorization_required"], true);
        assert!(!output.to_string().contains(root.to_str().unwrap()));
        for (key, value) in [
            ("family", json!("gateway.users")),
            ("effects", json!(true)),
            ("sql", json!(["DROP TABLE gateway.users"])),
        ] {
            let mut bad = plan.clone();
            bad[key] = value;
            assert!(review(&bad, &files).is_err());
        }
        let mut cutover = plan.clone();
        cutover["operation"] = json!({"operation":"cutover"});
        cutover["expected_revision"] = json!(7);
        let five = files
            .iter()
            .filter(|(k, _)| {
                !matches!(
                    k.as_str(),
                    "authority_snapshot" | "native_writer_acceptance"
                )
            })
            .map(|(k, f)| (k.clone(), f.sha256.clone()))
            .collect::<BTreeMap<_, _>>();
        let scope = safe::sha256_bytes(&serde_json::to_vec(&five).unwrap());
        cutover["sql"] = json!([format!(
            "SELECT partition_ops.cutover('gateway.audit_logs',7,'{scope}')"
        )]);
        assert!(review(&cutover, &files).is_ok());
        fs::write(
            &files["native_writer_acceptance"].path,
            b"modified artifact",
        )
        .unwrap();
        assert!(review(&plan, &files).is_err());
        files.remove("native_writer_acceptance");
        assert!(review(&plan, &files).is_err());
    }
}
