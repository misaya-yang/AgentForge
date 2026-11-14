//! Decide whether a saved page needs a dataset write. Unchanged hashes are skipped.
use crate::migration_knowledge_query::document_acl::{self, Acl, Clause};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexSnapshot {
    pub deleted: bool,
    pub document_id: Option<String>,
    pub document_status: String,
    pub document_archived: bool,
    pub document_version: i64,
    pub indexed_sha: Option<String>,
    pub content_sha: String,
    pub markdown: String,
    pub title: String,
    pub revision: i64,
    pub space_id: String,
    pub item_id: String,
    pub dataset_id: String,
    pub tenant_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexPlan {
    Skip,
    Archive { document_id: String },
    Create { markdown: String },
    Replace { document_id: String, expected_version: i32, markdown: String },
}

pub fn plan(snapshot: &IndexSnapshot) -> IndexPlan {
    let blank = snapshot.markdown.trim().is_empty();
    if snapshot.deleted || blank {
        return match snapshot.document_id.clone() {
            Some(document_id) if !snapshot.document_archived => IndexPlan::Archive { document_id },
            _ => IndexPlan::Skip,
        };
    }
    let same = snapshot.indexed_sha.as_deref() == Some(snapshot.content_sha.as_str())
        && snapshot.document_id.is_some()
        && !snapshot.document_archived
        && snapshot.document_status == "completed";
    if same {
        return IndexPlan::Skip;
    }
    if snapshot.document_status == "completed"
        && !snapshot.document_archived
        && snapshot.document_version > 0
        && let Some(document_id) = snapshot.document_id.clone()
        && let Ok(expected_version) = i32::try_from(snapshot.document_version)
    {
        return IndexPlan::Replace {
            document_id,
            expected_version,
            markdown: snapshot.markdown.clone(),
        };
    }
    IndexPlan::Create { markdown: snapshot.markdown.clone() }
}

pub fn document_metadata(snapshot: &IndexSnapshot, viewers: &[String]) -> Value {
    json!({
        "space_id": snapshot.space_id,
        "item_id": snapshot.item_id,
        "revision": snapshot.revision,
        "space_content_sha256": snapshot.content_sha,
        "_connector_acl": connector_acl(&snapshot.tenant_id, snapshot.revision, viewers),
    })
}

pub fn connector_acl(tenant: &str, revision: i64, viewers: &[String]) -> Value {
    let acl = Acl {
        schema: "connector-acl/v1".into(),
        verified: true,
        tenant_id: tenant.to_owned(),
        connection_id: "space".into(),
        source_revision: revision.to_string(),
        expires_at: chrono::Utc::now().timestamp() + 60 * 60 * 24 * 365 * 10,
        clauses: vec![Clause { users: viewers.to_vec(), roles: vec![] }],
    };
    serde_json::to_value(acl).unwrap_or(Value::Null)
}

pub fn viewer_can_retrieve(metadata: &Value, user_id: &str, tenant: &str, roles: &[String]) -> bool {
    let actor = crate::migration_knowledge_query::UserContext {
        user_id: user_id.into(),
        tenant_id: tenant.into(),
        user_tier: "normal".into(),
        user_type: "user".into(),
        roles: roles.to_vec(),
        is_authenticated: true,
    };
    document_acl::allowed("text", metadata, Some(&actor))
}

pub fn stamp_hit(hit: &mut Value, space_id: &str, item_id: &str, revision: i64) {
    hit["space_id"] = json!(space_id);
    hit["item_id"] = json!(item_id);
    hit["revision"] = json!(revision);
    if hit.get("metadata").and_then(Value::as_object).is_none() {
        hit["metadata"] = json!({});
    }
    hit["metadata"]["space_id"] = json!(space_id);
    hit["metadata"]["item_id"] = json!(item_id);
    hit["metadata"]["revision"] = json!(revision);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> IndexSnapshot {
        IndexSnapshot {
            deleted: false,
            document_id: None,
            document_status: String::new(),
            document_archived: false,
            document_version: 0,
            indexed_sha: None,
            content_sha: "abc".into(),
            markdown: "唯一短语".into(),
            title: "页".into(),
            revision: 1,
            space_id: "space".into(),
            item_id: "item".into(),
            dataset_id: "dataset".into(),
            tenant_id: "tenant".into(),
        }
    }

    #[test]
    fn spaces_index_skips_unchanged_hash_and_archives_deletes() {
        let mut fresh = snapshot();
        assert!(matches!(plan(&fresh), IndexPlan::Create { .. }));
        fresh.document_id = Some("doc".into());
        fresh.document_status = "completed".into();
        fresh.document_version = 2;
        fresh.indexed_sha = Some("abc".into());
        assert_eq!(plan(&fresh), IndexPlan::Skip);
        fresh.content_sha = "def".into();
        fresh.markdown = "changed".into();
        assert!(matches!(plan(&fresh), IndexPlan::Replace { expected_version: 2, .. }));
        fresh.deleted = true;
        assert_eq!(plan(&fresh), IndexPlan::Archive { document_id: "doc".into() });
        let metadata = document_metadata(&snapshot(), &["owner".into()]);
        assert!(viewer_can_retrieve(&metadata, "owner", "tenant", &[]));
        assert!(!viewer_can_retrieve(&metadata, "stranger", "tenant", &[]));
        let mut hit = serde_json::json!({"text":"唯一短语","metadata":{}});
        stamp_hit(&mut hit, "space", "item", 4);
        assert_eq!(hit["item_id"], "item");
        assert_eq!(hit["metadata"]["revision"], 4);
    }
}
