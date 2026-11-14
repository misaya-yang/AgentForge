//! Canonical 031 -> 032 extension. Source validation never manufactures a DB fingerprint.
use crate::{AuthorityError, Result, manifest};
use serde_json::{Value, json};
use std::{fs, path::Path};

pub const EPOCH_DIR: &str = "database/migrations/2026_08_post_kb_v1";
pub const ANCHOR: &str = "database/authority/anchors/031_manifest.json";
const ANCHOR_SHA: &str = "3f76b8a7387f6b441421fd6c152ca1c50c3244646872b7910652e1dc0e5abbc1";

fn refused(message: &str) -> AuthorityError {
    AuthorityError::Refused(message.into())
}

pub fn verify_installed(root: &Path) -> Result<Value> {
    super::catalog_connections_031::verify_installed(root)?;
    if manifest::file_sha256(&root.join(ANCHOR))? != ANCHOR_SHA {
        return Err(refused("032 predecessor manifest changed"));
    }
    let anchor: Value = serde_json::from_slice(&fs::read(root.join(ANCHOR))?)
        .map_err(|_| refused("032 anchor malformed"))?;
    let active = manifest::load_epoch_manifest(&root.join(EPOCH_DIR).join("manifest.yml"))?;
    if active.epoch != 32 || active.changes.len() != 32 {
        return Err(refused(
            "032 requires the exact contiguous epoch-31 prefix plus one successor",
        ));
    }
    for (old, new) in anchor["changes"]
        .as_array()
        .ok_or_else(|| refused("032 anchor malformed"))?
        .iter()
        .zip(&active.changes)
    {
        if *old != serde_json::to_value(new).map_err(|_| refused("032 manifest entry malformed"))? {
            return Err(refused("032 rewrote its predecessor manifest entry"));
        }
    }
    let change = &active.changes[31];
    let sql = fs::read_to_string(root.join(EPOCH_DIR).join(&change.file))?;
    if change.name != "gateway_automations"
        || change.file != "032_gateway_automations.sql"
        || change.sha256 != manifest::file_sha256(&root.join(EPOCH_DIR).join(&change.file))?
        || change.owner != "owner"
        || change.postconditions.is_empty()
        || change.transaction_mode != manifest::TransactionMode::Transactional
        || !sql.contains("GRANT SELECT,INSERT,UPDATE ON")
        || sql.contains("GRANT DELETE")
        || sql.contains("GRANT ALL")
    {
        return Err(refused(
            "032 catalog SQL or minimum-privilege contract changed",
        ));
    }
    crate::sql::reject_embedded_transactions(&sql, &change.name)?;
    let reference: Value = serde_json::from_slice(&fs::read(
        root.join(EPOCH_DIR).join("032_verification.json"),
    )?)
    .map_err(|_| refused("032 reference malformed"))?;
    if reference["sequence"] != 32
        || reference["sql_sha256"] != change.sha256
        || reference["predecessor_manifest_sha256"] != ANCHOR_SHA
    {
        return Err(refused(
            "032 verification does not bind the reviewed predecessor and SQL",
        ));
    }
    Ok(
        json!({"source_plan_sha256":change.sha256,"predecessor_epoch":31,"canonical_epoch":32,
        "candidate_epoch":32,"database_actions":false,"fingerprint_measurement":reference["fingerprint_measurement"]}),
    )
}

/// Uses the existing isolated, non-superuser, exact-prefix measurement runner.
pub async fn measure(root: &Path, expected_database: &str, approved: bool) -> Result<()> {
    verify_installed(root)?;
    let baseline_path = root.join("database/baselines/2026_08_post_kb_v1/manifest.json");
    let baseline = manifest::load_baseline_manifest(&baseline_path)?;
    let baseline_sha = manifest::file_sha256(&baseline_path)?;
    let epoch = manifest::load_epoch_manifest(&root.join(EPOCH_DIR).join("manifest.yml"))?;
    let reference_path = root.join(EPOCH_DIR).join("032_verification.json");
    let mut reference: Value = serde_json::from_slice(&fs::read(&reference_path)?)
        .map_err(|_| refused("032 reference malformed"))?;
    if reference["fingerprint_measurement"]["status"] != "pending" {
        return Err(refused("032 measured fingerprints are immutable"));
    }
    super::measure_successors::measure_single_step(
        &root.join("database"), expected_database, 31, 32, &baseline, &baseline_sha,
        &epoch, approved, |evidence| {
            let mut fingerprints = evidence.raw_fingerprints.clone();
            fingerprints.insert("acl".into(), evidence.base_acl_sha256.clone());
            reference["baseline_manifest_sha256"] = json!(baseline_sha);
            reference["fingerprints"] = json!(fingerprints);
            reference["raw_fingerprints"] = json!(evidence.raw_fingerprints);
            reference["fingerprint_measurement"] = json!({"status":"measured","tool":"platform-migration-authority/measure-gateway-automations-032","engine":"PostgreSQL 16","database":expected_database,"predecessor_epoch":31,"candidate_epoch":32});
            reference["acl_compatibility"] = json!({"revision":crate::ledger_acl::REVISION,
                "manifest_sha256":crate::ledger_acl::MANIFEST_SHA256,
                "delta_line":"schema_acl:public:migrator:USAGE:grantopt=0",
                "base_acl_sha256":evidence.base_acl_sha256,"effective_acl_sha256":evidence.raw_acl_sha256,
                "removed_delta_count":evidence.raw_acl_lines.len() - evidence.base_acl_lines.len(),
                "raw_acl_lines":evidence.raw_acl_lines,"base_acl_lines":evidence.base_acl_lines});
            fs::write(&reference_path, format!("{}\n", serde_json::to_string_pretty(&reference).map_err(|_| refused("032 reference serialization failed"))?))?;
            Ok(())
        },
    ).await?;
    Ok(())
}

pub fn admission_checks(suffix: &str) -> Result<Vec<String>> {
    if ![
        "gateway",
        "knowledge_api",
        "knowledge_worker",
        "runtime",
        "capability_worker",
    ]
    .contains(&suffix)
    {
        return Err(refused("invalid automation principal suffix"));
    }
    Ok(vec![format!(
        r#"WITH ids AS (
      SELECT (left(current_user,length(current_user)-length('{suffix}'))||'gateway')::regrole::oid AS gateway_oid,
             (left(current_user,length(current_user)-length('{suffix}'))||'owner')::regrole::oid AS owner_oid
    ), rels AS (SELECT c.relowner,c.relacl FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='gateway' AND c.relname=ANY(ARRAY['automation_rules','automation_deliveries','automation_notifications']) AND c.relkind='r')
    SELECT (SELECT count(*) FROM rels)=3
      AND NOT EXISTS(SELECT 1 FROM rels r,ids i WHERE r.relowner<>i.owner_oid)
      AND NOT EXISTS(SELECT 1 FROM rels r,ids i CROSS JOIN LATERAL aclexplode(r.relacl) acl WHERE acl.grantee NOT IN (i.gateway_oid,i.owner_oid) OR acl.is_grantable)
      AND NOT EXISTS(SELECT 1 FROM rels r,ids i WHERE (SELECT array_agg(a.privilege_type ORDER BY a.privilege_type) FROM aclexplode(r.relacl) a WHERE a.grantee=i.gateway_oid) IS DISTINCT FROM ARRAY['INSERT','SELECT','UPDATE']::text[])
    "#
    )])
}

#[cfg(test)]
mod tests {
    #[test]
    fn formal_032_preserves_the_031_prefix_and_declares_minimum_privileges() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        assert_eq!(
            super::verify_installed(&root).unwrap()["canonical_epoch"],
            32
        );
        assert_eq!(super::admission_checks("gateway").unwrap().len(), 1);
        assert!(super::admission_checks("owner").is_err());
    }
}
