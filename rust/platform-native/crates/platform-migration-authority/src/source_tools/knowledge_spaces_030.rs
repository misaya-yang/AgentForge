//! Knowledge spaces successor candidate. The measured chain stays at epoch 29.
//! Sequences 30-35 belong to the closed service-successor catalog, so this
//! module validates the proposal and refuses to rewrite frozen fingerprints.
use super::service_successors;
use crate::{distribution, AuthorityError, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const PROPOSAL: &str = "docs/design/knowledge-spaces-030.sql.proposal";
pub const CANDIDATE_EPOCH: i32 = 30;
const PROPOSAL_SQL: &str = include_str!("../../../../../../docs/design/knowledge-spaces-030.sql.proposal");
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
    PROPOSAL_SQL
}

pub fn proposal_sha256() -> String {
    format!("{:x}", Sha256::digest(PROPOSAL_SQL.as_bytes()))
}

pub fn business_ddl() -> Result<&'static str> {
    slice_between("BEGIN BUSINESS DDL", "END BUSINESS DDL")
}

/// Why this candidate cannot be copied into the pinned epoch-29 distribution.
pub fn activation_refusal() -> &'static str {
    "knowledge spaces successor 030 stays a source proposal: database/ authority \
     distribution is pinned at measured epoch 29, and sequences 030-035 are the \
     closed service-successor catalog. Installing 030_knowledge_spaces.sql or \
     advancing manifest.yml would change frozen fingerprints. Measurement is \
     separate and requires an isolated PostgreSQL 16 owner run."
}

pub fn validate_proposal() -> Result<()> {
    if !PROPOSAL_SQL.starts_with("-- Knowledge spaces successor candidate 030.") {
        return Err(refused("030 proposal header is missing"));
    }
    if PROPOSAL_SQL.contains("BEGIN") && PROPOSAL_SQL.contains("COMMIT") {
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
        != "141024ba9914bf78ec9b3796966fb270001d786204296253bb23c7a904559a2c"
    {
        return Err(refused("030 service-successor catalog pin changed"));
    }
    Ok(())
}

/// Refuses to place the candidate inside the pinned database package.
pub fn refuse_distribution_install(database_root: &Path) -> Result<()> {
    distribution::verify(database_root, "2026_08_post_kb_v1")?;
    if database_root
        .join("migrations/2026_08_post_kb_v1/030_knowledge_spaces.sql")
        .exists()
    {
        return Err(refused(
            "030 SQL must not enter the pinned migration directory before measurement",
        ));
    }
    Err(AuthorityError::Refused(activation_refusal().into()))
}

fn slice_between(start: &str, end: &str) -> Result<&'static str> {
    let Some(from) = PROPOSAL_SQL.find(start) else {
        return Err(refused("030 proposal marker is missing"));
    };
    let rest = &PROPOSAL_SQL[from + start.len()..];
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
    fn pinned_distribution_refuses_unmeasured_030_install() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .join("database");
        let error = refuse_distribution_install(&root).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("epoch 29"), "{message}");
        assert!(message.contains("030-035"), "{message}");
        assert!(!root
            .join("migrations/2026_08_post_kb_v1/030_knowledge_spaces.sql")
            .exists());
    }
}
