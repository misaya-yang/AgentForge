//! Source Agent CRUD/draft/version contracts through the concrete native repository.
//! Database tests require explicit invocation; no skipped DB test is a release pass.
mod agent_studio_support;
use agent_studio_support::*;
use ai_platform_gateway::migration_agent::{spec as contract, studio::ListAgents};
use serde_json::{Value, json};
#[test]
fn copy_excludes_secret_and_resource_state() {
    let mut original = spec("Answer clearly");
    original["model"]["max_tokens"] = json!(2048);
    for (k, v) in [
        (
            "capabilities",
            json!([{"type":"mcp","resource_id":"server-a"}]),
        ),
        ("knowledge", json!([{"dataset_id":"dataset-a"}])),
        ("skills", json!([{"skill_id":"skill-a"}])),
        ("connectors", json!([{"connection_id":"connector-a"}])),
        ("tool_bindings", json!([{"resource_id":"tool-a"}])),
        (
            "nested",
            json!({"safe":true,"secretRef":"synthetic-reference"}),
        ),
        ("apiKey", json!("synthetic-not-a-real-key")),
    ] {
        original[k] = v;
    }
    let copy = contract::copy_spec(&original);
    assert_eq!(copy["model"]["max_tokens"], 2048);
    assert_eq!(copy["capabilities"], json!([]));
    assert_eq!(copy["knowledge"], json!([]));
    assert_eq!(copy["memory"], json!({}));
    for excluded in [
        "skills",
        "connectors",
        "tool_bindings",
        "nested",
        "apiKey",
        "secretRef",
    ] {
        assert!(!copy.to_string().contains(excluded));
    }
}
#[test]
fn unsafe_specs_and_legacy_read_redaction() {
    for key in [
        "apiKey",
        "secretRef",
        "private_key",
        "authorization",
        "bearer_token",
    ] {
        let mut s = spec("safe");
        s["memory"][key] = json!("synthetic-not-a-real-secret");
        assert!(!contract::safety_errors(&s).is_empty());
    }
    let mut s = spec("safe");
    s["apiKey"] = json!("synthetic-not-a-real-key");
    s["secretRef"] = json!("synthetic-reference");
    s["tool_bindings"] = json!([{"resource_id":"legacy-tool"}]);
    s["model"]["private_key"] = json!("synthetic-private-material");
    s["memory"]["authorization"] = json!("synthetic-authorization");
    let rendered = contract::redact_for_read(&s).to_string();
    for field in [
        "apiKey",
        "secretRef",
        "tool_bindings",
        "private_key",
        "authorization",
        "synthetic-not-a-real-key",
        "synthetic-private-material",
    ] {
        assert!(!rendered.contains(field));
    }
}
fn query(limit: i64, cursor: Option<String>) -> ListAgents {
    ListAgents {
        limit,
        cursor,
        status: None,
        owner_id: None,
        search: None,
        channel: None,
    }
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL contract"]
async fn create_list_get_and_cursor_pagination() -> Result<()> {
    fixture(|pool, repo| async move {
        user(&pool, "owner-a", "tenant-a").await?;
        let owner = actor("owner-a", "tenant-a");
        let a = repo
            .create(
                &owner,
                "First",
                None,
                "First agent",
                &spec("Answer clearly"),
            )
            .await?;
        let b = repo
            .create(
                &owner,
                "Second",
                None,
                "Second agent",
                &spec("Answer clearly"),
            )
            .await?;
        assert_ne!(a["agent_id"], b["agent_id"]);
        assert_eq!(
            repo.get(&owner, a["agent_id"].as_str().unwrap()).await?["name"],
            "First"
        );
        let first = repo.list(&owner, query(1, None)).await?;
        assert_eq!(first["items"].as_array().unwrap().len(), 1);
        let cursor = first["next_cursor"].as_str().unwrap().to_owned();
        let second = repo.list(&owner, query(1, Some(cursor))).await?;
        assert_eq!(second["items"].as_array().unwrap().len(), 1);
        assert_ne!(
            first["items"][0]["agent_id"],
            second["items"][0]["agent_id"]
        );
        Ok(())
    })
    .await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL contract"]
async fn draft_conflict_preserves_newer_edit_and_atomic_metadata() -> Result<()> {
    fixture(|pool, repo| async move {
        user(&pool, "owner-a", "tenant-a").await?;
        let owner = actor("owner-a", "tenant-a");
        let a = repo
            .create(
                &owner,
                "Initial",
                None,
                "Initial description",
                &spec("Initial"),
            )
            .await?;
        let id = a["agent_id"].as_str().unwrap();
        let saved = repo
            .update_draft(
                &owner,
                id,
                1,
                &spec("newer edit"),
                &json!({"name":"Atomic save","description":"Committed together"}),
            )
            .await?;
        assert_eq!(saved["revision"], 2);
        assert_eq!(
            repo.update_draft(
                &owner,
                id,
                1,
                &spec("stale"),
                &json!({"name":"Must not save"})
            )
            .await
            .unwrap_err()
            .status,
            409
        );
        assert_eq!(repo.get(&owner, id).await?["name"], "Atomic save");
        assert_eq!(
            repo.draft(&owner, id).await?["spec"]["instructions"],
            "newer edit"
        );
        let mut unsafe_spec = spec("unsafe");
        unsafe_spec["memory"]["password"] = json!("synthetic-secret");
        assert!(
            repo.update_draft(
                &owner,
                id,
                2,
                &unsafe_spec,
                &json!({"name":"Must not save"})
            )
            .await
            .is_err()
        );
        assert_eq!(repo.get(&owner, id).await?["name"], "Atomic save");
        let next = repo
            .update_draft(
                &owner,
                id,
                2,
                &spec("retry edit"),
                &json!({"name":"Atomic retry"}),
            )
            .await?;
        assert_eq!(next["revision"], 3);
        assert_eq!(repo.get(&owner, id).await?["name"], "Atomic retry");
        Ok(())
    })
    .await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL contract"]
async fn version_remains_immutable_after_later_draft_edit() -> Result<()> {
    fixture(|pool,repo|async move{user(&pool,"owner-a","tenant-a").await?;let owner=actor("owner-a","tenant-a");let a=repo.create(&owner,"Test",None,"",&spec("Answer clearly")).await?;let id=a["agent_id"].as_str().unwrap();let version=repo.create_version(&owner,id,1).await?;let hash=version["spec_hash"].clone();repo.update_draft(&owner,id,1,&spec("changed after version"),&json!({})).await?;let versions=repo.versions(&owner,id).await?;assert_eq!(versions[0]["spec_hash"],hash);assert_eq!(versions[0]["spec"]["instructions"],"Answer clearly");let stored:Value=sqlx::query_scalar("SELECT to_jsonb(v) FROM agent_versions v WHERE tenant_id='tenant-a' AND agent_id=$1::uuid").bind(id).fetch_one(&pool).await?;assert_eq!(stored["resolved_spec"]["instructions"],"Answer clearly");assert_eq!(stored["source_draft_revision"],1);Ok(())}).await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL contract"]
async fn archive_makes_draft_readonly_and_delete_is_soft() -> Result<()> {
    fixture(|pool,repo|async move{user(&pool,"owner-a","tenant-a").await?;let owner=actor("owner-a","tenant-a");let a=repo.create(&owner,"Test",None,"",&spec("Answer clearly")).await?;let id=a["agent_id"].as_str().unwrap();assert_eq!(repo.archive(&owner,id,true).await?["status"],"archived");error(repo.update_draft(&owner,id,1,&spec("should fail"),&json!({})).await,409,"AGENT_ARCHIVED");repo.soft_delete(&owner,id).await?;assert_eq!(repo.get(&owner,id).await.unwrap_err().status,404);let deleted:bool=sqlx::query_scalar("SELECT deleted_at IS NOT NULL FROM agents WHERE tenant_id='tenant-a' AND agent_id=$1::uuid").bind(id).fetch_one(&pool).await?;assert!(deleted);Ok(())}).await
}
