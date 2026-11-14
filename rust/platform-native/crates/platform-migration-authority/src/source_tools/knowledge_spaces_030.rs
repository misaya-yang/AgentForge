//! Installed knowledge-spaces successor 030.
//! The frozen epoch-29 lock, baselines, and 029 fingerprints stay unchanged.
//! Sequences 30-35 in the service-successor catalog remain that closed preview.
use super::service_successors;
use crate::{manifest, AuthorityError, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

pub const MIGRATION: &str = "database/migrations/2026_08_post_kb_v1/030_knowledge_spaces.sql";
pub const VERIFICATION: &str = "database/migrations/2026_08_post_kb_v1/030_verification.json";
pub const CANDIDATE_EPOCH: i32 = 30;
const LOCK_SHA256: &str = "b8580e5d3ee6d8fa25966e9857d5372082e664788616db91cac2f678fb10f87b";
const MANIFEST_SHA256: &str = "2780972d37314780d96012a8bdb1805b4e3a50118b8d61956ce645c10f4832fa";
const ARCHIVED_MANIFEST: &str =
    "docs/design/canonical-schema-service-successors-approved29-manifest.yml";
const ARCHIVED_LOCK: &str =
    "docs/design/canonical-schema-service-successors-approved29-package-lock.json";
const MIGRATION_SQL: &str = include_str!(
    "../../../../../../database/migrations/2026_08_post_kb_v1/030_knowledge_spaces.sql"
);
const TABLES: [&str; 7] = [
    "knowledge.spaces",
    "knowledge.space_items",
    "knowledge.space_item_revisions",
    "knowledge.space_members",
    "knowledge.space_item_shares",
    "knowledge.space_item_stars",
    "knowledge.space_item_activity",
];

pub fn proposal_sql() -> &'static str {
    MIGRATION_SQL
}

pub fn proposal_sha256() -> String {
    format!("{:x}", Sha256::digest(MIGRATION_SQL.as_bytes()))
}

pub fn business_ddl() -> Result<&'static str> {
    slice_between("BEGIN BUSINESS DDL", "END BUSINESS DDL")
}

pub fn validate_proposal() -> Result<()> {
    if !MIGRATION_SQL.starts_with("-- Knowledge spaces successor candidate 030.") {
        return Err(refused("030 proposal header is missing"));
    }
    if MIGRATION_SQL.contains("BEGIN") && MIGRATION_SQL.contains("COMMIT") {
        let business = business_ddl()?;
        if business.contains("\nBEGIN;") || business.contains("\nCOMMIT;") {
            return Err(refused("030 business DDL must not own a transaction"));
        }
    }
    let _ = slice_between("BEGIN OWNER GUARD", "END OWNER GUARD")?;
    let business = business_ddl()?;
    let grants = slice_between("BEGIN GRANTS", "END GRANTS")?;
    for table in TABLES {
        if !business.contains(&format!("CREATE TABLE {table} (")) {
            return Err(refused("030 proposal is missing a required table"));
        }
        if !business.contains(&format!("REVOKE ALL ON TABLE {table} FROM PUBLIC")) {
            return Err(refused("030 proposal leaves a table granted to PUBLIC"));
        }
    }
    for column in [
        "dataset_id varchar(255) NOT NULL REFERENCES knowledge.datasets (dataset_id)",
        "ydoc_state bytea",
        "title_tsv tsvector",
        "content_sha256 char(64) NOT NULL",
        "principal_type varchar(16) NOT NULL",
        "deleted_at timestamptz",
    ] {
        if !business.contains(column) {
            return Err(refused("030 proposal is missing a required column"));
        }
    }
    if business.contains("GRANT ") || grants.contains("GRANT DELETE") || grants.contains("GRANT ALL")
    {
        return Err(refused("030 grants must be API select/insert/update and worker column select only"));
    }
    if !grants.contains("GRANT SELECT, INSERT, UPDATE ON TABLE") || !grants.contains("knowledge_api")
    {
        return Err(refused("030 Knowledge API grant is missing"));
    }
    if !grants.contains("GRANT SELECT (space_id, tenant_id, dataset_id, archived_at)")
        || !grants.contains("knowledge_worker")
    {
        return Err(refused("030 Knowledge worker column grant is missing"));
    }
    if grants.contains("gateway") || grants.contains("runtime") || grants.contains("DELETE") {
        return Err(refused("030 grant scope is wider than Knowledge API and worker read"));
    }
    if service_successors::CATALOG_SHA256
        != "6105327d3397b1c2d3a30cb92f9a3faf300f27813db4be7c96a9e23087772a29"
    {
        return Err(refused("030 service-successor catalog pin changed"));
    }
    Ok(())
}

/// Recognizes the installed successor without rewriting the frozen 029 lock.
pub fn verify_installed(root: &Path) -> Result<Value> {
    validate_proposal()?;
    let root = root.canonicalize().map_err(|_| refused("030 source root is unavailable"))?;
    if manifest::file_sha256(&root.join(ARCHIVED_LOCK))? != LOCK_SHA256
        || manifest::file_sha256(&root.join(ARCHIVED_MANIFEST))? != MANIFEST_SHA256
        || manifest::file_sha256(&root.join("database/authority/package-authority-lock.json"))? != LOCK_SHA256
    {
        return Err(refused("030 predecessor archive or frozen lock changed"));
    }
    let lock: Value = serde_json::from_slice(&fs::read(root.join("database/authority/package-authority-lock.json"))?)
        .map_err(|_| refused("030 frozen lock is unreadable"))?;
    let files = lock["files_sha256"].as_object().ok_or_else(|| refused("030 frozen lock has no inventory"))?;
    for (relative, expected) in files {
        if relative == "migrations/2026_08_post_kb_v1/manifest.yml" {
            continue;
        }
        let actual = manifest::file_sha256(&root.join("database").join(relative))?;
        if actual != expected.as_str().unwrap_or("") {
            return Err(refused("030 changed a frozen predecessor input"));
        }
    }
    let live = manifest::load_epoch_manifest(&root.join("database/migrations/2026_08_post_kb_v1/manifest.yml"))?;
    let archived: Value = serde_json::from_slice(&fs::read(root.join(ARCHIVED_MANIFEST))?)
        .map_err(|_| refused("030 predecessor manifest is unreadable"))?;
    let archived_changes = archived["changes"].as_array().ok_or_else(|| refused("030 predecessor manifest has no changes"))?;
    if !(30..=32).contains(&live.epoch)
        || live.changes.len() != live.epoch as usize
        || archived["epoch"] != 29
        || archived_changes.len() != 29
    {
        return Err(refused("030 manifest is not the epoch-29 archive plus one change"));
    }
    for (left, right) in archived_changes.iter().zip(live.changes.iter()) {
        if left["sequence"].as_i64() != Some(right.sequence as i64)
            || left["file"].as_str() != Some(right.file.as_str())
            || left["sha256"].as_str() != Some(right.sha256.as_str())
        {
            return Err(refused("030 rewrote a frozen predecessor change"));
        }
    }
    let installed = live.changes.get(29).ok_or_else(|| refused("030 change is missing"))?;
    let sql_sha = proposal_sha256();
    if installed.sequence != 30
        || installed.name != "knowledge_spaces"
        || installed.file != "030_knowledge_spaces.sql"
        || installed.sha256 != sql_sha
        || installed.owner != "owner"
        || installed.postconditions.is_empty()
    {
        return Err(refused("030 manifest entry does not match the installed SQL"));
    }
    let verification: Value = serde_json::from_slice(&fs::read(root.join(VERIFICATION))?)
        .map_err(|_| refused("030 verification is unreadable"))?;
    if verification["sequence"] != 30
        || verification["predecessor_epoch"] != 29
        || verification["predecessor_manifest_sha256"] != MANIFEST_SHA256
        || verification["predecessor_lock_sha256"] != LOCK_SHA256
        || verification["sql_sha256"] != sql_sha
    {
        return Err(refused("030 verification does not bind the installed SQL"));
    }
    Ok(json!({
        "source_plan_sha256": sql_sha,
        "predecessor_epoch": 29,
        "canonical_epoch": 30,
        "candidate_epoch": 30,
        "database_actions": false
    }))
}

fn slice_between(start: &str, end: &str) -> Result<&'static str> {
    let Some(from) = MIGRATION_SQL.find(start) else {
        return Err(refused("030 proposal marker is missing"));
    };
    let rest = &MIGRATION_SQL[from + start.len()..];
    let Some(to) = rest.find(end) else {
        return Err(refused("030 proposal marker is unbalanced"));
    };
    Ok(rest[..to].trim())
}

fn refused(message: &str) -> AuthorityError {
    AuthorityError::Refused(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposal_matches_the_knowledge_space_model_and_minimal_acl() {
        validate_proposal().unwrap();
        let ddl = business_ddl().unwrap();
        assert!(ddl.contains("kind IN ('personal', 'team', 'tenant')"));
        assert!(ddl.contains("kind IN ('page', 'folder', 'file', 'link', 'slide', 'site', 'image', 'external')"));
        assert!(ddl.contains("role IN ('owner', 'editor', 'commenter', 'viewer')"));
        assert!(ddl.contains("content_format IN ('blocknote_json', 'markdown', 'binary')"));
        assert!(ddl.contains("revoked_at timestamptz"));
        assert!(!ddl.contains("GRANT DELETE") && !ddl.contains("DELETE FROM"));
        assert!(proposal_sha256().len() == 64);
    }

    #[test]
    fn installed_030_is_recognized_without_rewriting_the_frozen_lock() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let report = verify_installed(&root).unwrap();
        assert_eq!(report["canonical_epoch"], 30);
        assert_eq!(report["predecessor_epoch"], 29);
        assert_eq!(report["source_plan_sha256"], proposal_sha256());
        assert!(root.join(MIGRATION).is_file());
        assert!(root.join(VERIFICATION).is_file());
    }
}
