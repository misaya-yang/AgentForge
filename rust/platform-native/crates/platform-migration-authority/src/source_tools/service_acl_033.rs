//! Canonical 032 -> 033 minimal service ACL correction. Source validation never
//! manufactures a DB fingerprint; measurement uses the exact single-step runner.
use crate::{AuthorityError, Result, manifest};
use serde_json::{Value, json};
use std::{fs, path::Path};

pub const EPOCH_DIR: &str = "database/migrations/2026_08_post_kb_v1";
pub const ANCHOR: &str = "database/authority/anchors/032_manifest.json";
const ANCHOR_SHA: &str = "01cf85a39213441e7c1e74be780f7e4adf00fc62674d4a6c3207476f7d6b53c7";
pub const FILE: &str = "033_service_acl_corrections.sql";
pub const NAME: &str = "service_acl_corrections";
/// Exact fresh/legacy ACL matrix transition: the first line replaces the
/// predecessor Gateway INSERT,UPDATE record; the other two are new records.
pub const REPLACED_ACL_LINE: &str =
    "acl:gateway.usage_hourly_aggregates:gateway:INSERT,UPDATE:grantopt=0";
pub const SERVICE_ACL_LINES: [&str; 3] = [
    "acl:gateway.usage_hourly_aggregates:gateway:INSERT,SELECT,UPDATE:grantopt=0",
    "acl:knowledge.kb_document_batch_items:knowledge_worker:SELECT,UPDATE:grantopt=0",
    "acl:knowledge.kb_document_batch_operations:knowledge_worker:SELECT,UPDATE:grantopt=0",
];
const SERVICE_ACL_CHECK: &str = r#"WITH ids AS (
 SELECT (left(current_user,length(current_user)-length('{suffix}'))||'gateway')::regrole::oid AS gateway_oid,
        (left(current_user,length(current_user)-length('{suffix}'))||'knowledge_worker')::regrole::oid AS worker_oid,
        (left(current_user,length(current_user)-length('{suffix}'))||'owner')::regrole::oid AS owner_oid
), rels AS (
 SELECT c.oid,n.nspname,c.relname,c.relowner,c.relacl FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
 WHERE (n.nspname,c.relname) IN (('gateway','usage_hourly_aggregates'),('knowledge','kb_document_batch_operations'),('knowledge','kb_document_batch_items')) AND c.relkind='r'
), privileges AS (
 SELECT r.nspname,r.relname,a.grantee,a.grantor,a.privilege_type,a.is_grantable FROM rels r CROSS JOIN LATERAL pg_catalog.aclexplode(r.relacl) a
)
SELECT (SELECT count(*) FROM rels)=3
 AND NOT EXISTS(SELECT 1 FROM rels r,ids i WHERE r.relowner<>i.owner_oid)
 AND NOT EXISTS(SELECT 1 FROM privileges p,ids i WHERE p.grantee IN (i.gateway_oid,i.worker_oid,0) AND (p.is_grantable OR p.grantor<>i.owner_oid OR p.grantee=0))
 AND (SELECT array_agg(p.privilege_type ORDER BY p.privilege_type) FROM privileges p,ids i WHERE p.relname='usage_hourly_aggregates' AND p.grantee=i.gateway_oid) IS NOT DISTINCT FROM ARRAY['INSERT','SELECT','UPDATE']::text[]
 AND NOT EXISTS(SELECT 1 FROM privileges p,ids i WHERE p.relname='usage_hourly_aggregates' AND p.grantee=i.worker_oid)
 AND (SELECT array_agg(p.privilege_type ORDER BY p.privilege_type) FROM privileges p,ids i WHERE p.relname='kb_document_batch_operations' AND p.grantee=i.worker_oid) IS NOT DISTINCT FROM ARRAY['SELECT','UPDATE']::text[]
 AND (SELECT array_agg(p.privilege_type ORDER BY p.privilege_type) FROM privileges p,ids i WHERE p.relname='kb_document_batch_items' AND p.grantee=i.worker_oid) IS NOT DISTINCT FROM ARRAY['SELECT','UPDATE']::text[]
 AND NOT EXISTS(SELECT 1 FROM privileges p,ids i WHERE p.nspname='knowledge' AND p.grantee=i.gateway_oid)"#;

fn refused(message: &str) -> AuthorityError {
    AuthorityError::Refused(message.into())
}
fn checks(suffix: &str) -> Vec<String> {
    vec![format!("SELECT ({})", SERVICE_ACL_CHECK.replace("{suffix}", suffix))]
}
pub fn service_acl_postconditions() -> Vec<String> {
    checks("owner")
}
pub fn service_acl_correction_sql(sql: &str) -> Result<&str> {
    let start = "-- BEGIN REQUIRED SERVICE ACL CORRECTION\n";
    let end = "-- END REQUIRED SERVICE ACL CORRECTION";
    if sql.matches(start).count() != 1 || sql.matches(end).count() != 1 {
        return Err(refused("033 exact service ACL source boundary missing"));
    }
    let body = sql
        .split_once(start)
        .and_then(|(_, tail)| tail.split_once(end))
        .map(|(body, _)| body)
        .ok_or_else(|| refused("033 exact service ACL source order differs"))?;
    for grant in [
        "GRANT SELECT ON TABLE gateway.usage_hourly_aggregates TO %I",
        "GRANT SELECT,UPDATE ON TABLE knowledge.kb_document_batch_operations,knowledge.kb_document_batch_items TO %I",
    ] {
        if body.matches(grant).count() != 1 {
            return Err(refused("033 exact service ACL grant differs"));
        }
    }
    let upper = body.to_ascii_uppercase();
    if body.matches("EXECUTE format('GRANT ").count() != 2
        || upper.matches("GRANT ").count() != 2
        || upper.contains("GRANT ALL")
        || upper.contains("WITH GRANT OPTION")
        || upper.contains("TO PUBLIC")
        || upper.contains("INSERT")
        || upper.contains("DELETE")
        || upper.contains("REVOKE")
    {
        return Err(refused("033 service ACL exceeds the bounded correction"));
    }
    Ok(body)
}

pub fn verify_installed(root: &Path) -> Result<Value> {
    super::gateway_automations_032::verify_installed(root)?;
    if manifest::file_sha256(&root.join(ANCHOR))? != ANCHOR_SHA {
        return Err(refused("033 predecessor manifest changed"));
    }
    let anchor: Value = serde_json::from_slice(&fs::read(root.join(ANCHOR))?)
        .map_err(|_| refused("033 anchor malformed"))?;
    let active = manifest::load_epoch_manifest(&root.join(EPOCH_DIR).join("manifest.yml"))?;
    let previous = anchor["changes"]
        .as_array()
        .ok_or_else(|| refused("033 anchor malformed"))?;
    if active.epoch != 33 || active.changes.len() != 33 || previous.len() != 32 {
        return Err(refused(
            "033 requires the exact contiguous epoch-32 prefix plus one successor",
        ));
    }
    for (old, new) in previous.iter().zip(&active.changes) {
        if *old != serde_json::to_value(new).map_err(|_| refused("033 manifest entry malformed"))? {
            return Err(refused("033 rewrote its predecessor manifest entry"));
        }
    }
    let change = &active.changes[32];
    let path = root.join(EPOCH_DIR).join(&change.file);
    let sql = fs::read_to_string(&path)?;
    if change.sequence != 33
        || change.name != NAME
        || change.file != FILE
        || change.sha256 != manifest::file_sha256(&path)?
        || change.owner != "owner"
        || change.postconditions != service_acl_postconditions()
        || change.transaction_mode != manifest::TransactionMode::Transactional
    {
        return Err(refused("033 service ACL SQL or minimum-privilege contract changed"));
    }
    crate::sql::reject_embedded_transactions(&sql, &change.name)?;
    service_acl_correction_sql(&sql)?;
    let reference: Value = serde_json::from_slice(&fs::read(
        root.join(EPOCH_DIR).join("033_verification.json"),
    )?)
    .map_err(|_| refused("033 reference malformed"))?;
    if reference["sequence"] != 33
        || reference["sql_sha256"] != change.sha256
        || reference["predecessor_manifest_sha256"] != ANCHOR_SHA
    {
        return Err(refused(
            "033 verification does not bind the reviewed predecessor and SQL",
        ));
    }
    Ok(json!({"source_plan_sha256":change.sha256,"predecessor_epoch":32,"canonical_epoch":33,
        "candidate_epoch":33,"database_actions":false,"fingerprint_measurement":reference["fingerprint_measurement"]}))
}

/// Uses the existing isolated, non-superuser, exact-prefix measurement runner.
pub async fn measure(root: &Path, expected_database: &str, approved: bool) -> Result<()> {
    verify_installed(root)?;
    let baseline_path = root.join("database/baselines/2026_08_post_kb_v1/manifest.json");
    let baseline = manifest::load_baseline_manifest(&baseline_path)?;
    let baseline_sha = manifest::file_sha256(&baseline_path)?;
    let epoch = manifest::load_epoch_manifest(&root.join(EPOCH_DIR).join("manifest.yml"))?;
    let reference_path = root.join(EPOCH_DIR).join("033_verification.json");
    let mut reference: Value = serde_json::from_slice(&fs::read(&reference_path)?)
        .map_err(|_| refused("033 reference malformed"))?;
    if reference["fingerprint_measurement"]["status"] != "pending" {
        return Err(refused("033 measured fingerprints are immutable"));
    }
    super::measure_successors::measure_single_step(
        &root.join("database"), expected_database, 32, 33, &baseline, &baseline_sha,
        &epoch, approved, |evidence| {
            let mut fingerprints = evidence.raw_fingerprints.clone();
            fingerprints.insert("acl".into(), evidence.base_acl_sha256.clone());
            reference["baseline_manifest_sha256"] = json!(baseline_sha);
            reference["fingerprints"] = json!(fingerprints);
            reference["raw_fingerprints"] = json!(evidence.raw_fingerprints);
            reference["fingerprint_measurement"] = json!({"status":"measured","tool":"platform-migration-authority/measure-service-acl-033","engine":"PostgreSQL 16","database":expected_database,"predecessor_epoch":32,"candidate_epoch":33});
            reference["acl_compatibility"] = json!({"revision":crate::ledger_acl::REVISION,
                "manifest_sha256":crate::ledger_acl::MANIFEST_SHA256,
                "delta_line":"schema_acl:public:migrator:USAGE:grantopt=0",
                "base_acl_sha256":evidence.base_acl_sha256,"effective_acl_sha256":evidence.raw_acl_sha256,
                "removed_delta_count":evidence.raw_acl_lines.len() - evidence.base_acl_lines.len(),
                "raw_acl_lines":evidence.raw_acl_lines,"base_acl_lines":evidence.base_acl_lines});
            fs::write(&reference_path, format!("{}\n", serde_json::to_string_pretty(&reference).map_err(|_| refused("033 reference serialization failed"))?))?;
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
        return Err(refused("invalid service ACL principal suffix"));
    }
    Ok(checks(suffix))
}

#[cfg(test)]
mod tests {
    #[test]
    fn formal_033_preserves_the_032_prefix_and_declares_minimum_privileges() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        assert_eq!(super::verify_installed(&root).unwrap()["canonical_epoch"], 33);
        assert_eq!(super::admission_checks("gateway").unwrap().len(), 1);
        assert!(super::admission_checks("owner").is_err());
        let sql = "x\n-- BEGIN REQUIRED SERVICE ACL CORRECTION\nEXECUTE format('GRANT SELECT ON TABLE gateway.usage_hourly_aggregates TO %I', a);\nEXECUTE format('GRANT SELECT,UPDATE ON TABLE knowledge.kb_document_batch_operations,knowledge.kb_document_batch_items TO %I', b);\n-- END REQUIRED SERVICE ACL CORRECTION\n";
        assert!(super::service_acl_correction_sql(sql).is_ok());
        assert!(super::service_acl_correction_sql(&sql.replace("SELECT,UPDATE", "SELECT,UPDATE,INSERT")).is_err());
        assert!(super::service_acl_correction_sql(&sql.replace("%I', b);", "%I WITH GRANT OPTION', b);")).is_err());
    }
}
