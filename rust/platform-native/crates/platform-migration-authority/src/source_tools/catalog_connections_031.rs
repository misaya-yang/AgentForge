//! Canonical 030 -> 031 extension. Source validation never manufactures a DB fingerprint.
use crate::{AuthorityError, Result, manifest};
use serde_json::{Value, json};
use std::{fs, path::Path};

pub const EPOCH_DIR: &str = "database/migrations/2026_08_post_kb_v1";
pub const ANCHOR: &str = "database/authority/anchors/030_manifest.json";
const ANCHOR_SHA: &str = "e2bd087b82c2c350d8e9f5a990f5cd064c4f117e5cb5500e883f50070ac127f7";

fn refused(message: &str) -> AuthorityError {
    AuthorityError::Refused(message.into())
}

pub fn verify_installed(root: &Path) -> Result<Value> {
    super::knowledge_spaces_030::verify_installed(root)?;
    if manifest::file_sha256(&root.join(ANCHOR))? != ANCHOR_SHA {
        return Err(refused("031 predecessor manifest changed"));
    }
    let anchor: Value = serde_json::from_slice(&fs::read(root.join(ANCHOR))?)
        .map_err(|_| refused("031 anchor malformed"))?;
    let active = manifest::load_epoch_manifest(&root.join(EPOCH_DIR).join("manifest.yml"))?;
    if active.epoch != 31 || active.changes.len() != 31 {
        return Err(refused(
            "031 requires the exact contiguous epoch-30 prefix plus one successor",
        ));
    }
    for (old, new) in anchor["changes"]
        .as_array()
        .ok_or_else(|| refused("031 anchor malformed"))?
        .iter()
        .zip(&active.changes)
    {
        if *old != serde_json::to_value(new).map_err(|_| refused("031 manifest entry malformed"))? {
            return Err(refused("031 rewrote its predecessor manifest entry"));
        }
    }
    let change = &active.changes[30];
    let sql = fs::read_to_string(root.join(EPOCH_DIR).join(&change.file))?;
    if change.name != "catalog_connections"
        || change.file != "031_catalog_connections.sql"
        || change.sha256 != manifest::file_sha256(&root.join(EPOCH_DIR).join(&change.file))?
        || change.owner != "owner"
        || change.postconditions.is_empty()
        || change.transaction_mode != manifest::TransactionMode::Transactional
        || !sql.contains("GRANT SELECT, INSERT, UPDATE ON TABLE")
        || sql.contains("GRANT DELETE")
        || sql.contains("GRANT ALL")
    {
        return Err(refused(
            "031 catalog SQL or minimum-privilege contract changed",
        ));
    }
    crate::sql::reject_embedded_transactions(&sql, &change.name)?;
    let reference: Value = serde_json::from_slice(&fs::read(
        root.join(EPOCH_DIR).join("031_verification.json"),
    )?)
    .map_err(|_| refused("031 reference malformed"))?;
    if reference["sequence"] != 31
        || reference["sql_sha256"] != change.sha256
        || reference["predecessor_manifest_sha256"] != ANCHOR_SHA
    {
        return Err(refused(
            "031 verification does not bind the reviewed predecessor and SQL",
        ));
    }
    Ok(
        json!({"source_plan_sha256":change.sha256,"predecessor_epoch":30,"canonical_epoch":31,
        "candidate_epoch":31,"database_actions":false,"fingerprint_measurement":reference["fingerprint_measurement"]}),
    )
}

/// Uses the existing isolated, non-superuser, exact-prefix measurement runner.
pub async fn measure(root: &Path, expected_database: &str, approved: bool) -> Result<()> {
    verify_installed(root)?;
    let baseline_path = root.join("database/baselines/2026_08_post_kb_v1/manifest.json");
    let baseline = manifest::load_baseline_manifest(&baseline_path)?;
    let baseline_sha = manifest::file_sha256(&baseline_path)?;
    let epoch = manifest::load_epoch_manifest(&root.join(EPOCH_DIR).join("manifest.yml"))?;
    let reference_path = root.join(EPOCH_DIR).join("031_verification.json");
    let mut reference: Value = serde_json::from_slice(&fs::read(&reference_path)?)
        .map_err(|_| refused("031 reference malformed"))?;
    super::measure_successors::measure_single_step(
        &root.join("database"), expected_database, 30, 31, &baseline, &baseline_sha,
        &epoch, approved, |evidence| {
            let mut fingerprints = evidence.raw_fingerprints.clone();
            fingerprints.insert("acl".into(), evidence.base_acl_sha256.clone());
            reference["baseline_manifest_sha256"] = json!(baseline_sha);
            reference["fingerprints"] = json!(fingerprints);
            reference["raw_fingerprints"] = json!(evidence.raw_fingerprints);
            reference["fingerprint_measurement"] = json!({"status":"measured","tool":"platform-migration-authority/measure-catalog-connections-031","engine":"PostgreSQL 16","database":expected_database,"predecessor_epoch":30,"candidate_epoch":31});
            reference["acl_compatibility"] = json!({"revision":crate::ledger_acl::REVISION,
                "manifest_sha256":crate::ledger_acl::MANIFEST_SHA256,
                "delta_line":"schema_acl:public:migrator:USAGE:grantopt=0",
                "base_acl_sha256":evidence.base_acl_sha256,"effective_acl_sha256":evidence.raw_acl_sha256,
                "removed_delta_count":evidence.raw_acl_lines.len() - evidence.base_acl_lines.len(),
                "raw_acl_lines":evidence.raw_acl_lines,"base_acl_lines":evidence.base_acl_lines});
            fs::write(&reference_path, format!("{}\n", serde_json::to_string_pretty(&reference).map_err(|_| refused("031 reference serialization failed"))?))?;
            Ok(())
        },
    ).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn canonical_031_preserves_030_and_never_claims_an_unmeasured_fingerprint() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let result = super::verify_installed(&root).unwrap();
        assert_eq!(result["canonical_epoch"], 31);
        assert_eq!(result["database_actions"], false);
        assert!(
            !root
                .join("config/catalog/036_catalog_connections.sql")
                .exists()
        );
    }
}
