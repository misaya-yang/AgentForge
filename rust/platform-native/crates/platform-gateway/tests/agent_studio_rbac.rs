//! Role and tenant boundary matrix through the concrete native repository.
mod agent_studio_support;
use agent_studio_support::*;
use ai_platform_gateway::migration_agent::{Actor, studio::ListAgents};
use serde_json::json;
#[test]
fn unauthenticated_and_public_contexts_are_rejected() {
    error(
        Actor::default().require_tenant(),
        401,
        "AUTHENTICATION_REQUIRED",
    );
    error(
        actor("api-client", "public").require_tenant(),
        403,
        "TENANT_REQUIRED",
    );
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL contract"]
async fn viewer_readonly_editor_mutations_and_owner_boundaries() -> Result<()> {
    fixture(|p, repo| async move {
        let owner = actor("owner-a", "tenant-a");
        for u in ["owner-a", "viewer-a", "editor-a", "another-user"] {
            user(&p, u, "tenant-a").await?;
        }
        let a = repo
            .create(&owner, "Test", None, "", &spec("Initial"))
            .await?;
        let id = a["agent_id"].as_str().unwrap();
        repo.upsert_member(&owner, id, "user", "viewer-a", "viewer")
            .await?;
        repo.upsert_member(&owner, id, "user", "editor-a", "editor")
            .await?;
        let viewer = actor("viewer-a", "tenant-a");
        error(repo.copy(&viewer, id, None, None).await, 404, "AGENT_NOT_FOUND");
        repo.get(&viewer, id).await?;
        repo.draft(&viewer, id).await?;
        repo.versions(&viewer, id).await?;
        error(
            repo.update(&viewer, id, &json!({"name":"Denied"})).await,
            404,
            "AGENT_NOT_FOUND",
        );
        error(
            repo.update_draft(&viewer, id, 1, &spec("Denied"), &json!({}))
                .await,
            404,
            "AGENT_NOT_FOUND",
        );
        error(
            repo.create_version(&viewer, id, 1).await,
            404,
            "AGENT_NOT_FOUND",
        );
        error(
            repo.archive(&viewer, id, false).await,
            404,
            "AGENT_NOT_FOUND",
        );
        error(
            repo.upsert_member(&viewer, id, "user", "another-user", "editor")
                .await,
            404,
            "AGENT_NOT_FOUND",
        );
        error(repo.soft_delete(&viewer, id).await, 404, "AGENT_NOT_FOUND");
        let editor = actor("editor-a", "tenant-a");
        error(repo.copy(&editor, id, None, None).await, 404, "AGENT_NOT_FOUND");
        repo.update(&editor, id, &json!({"description":"Edited"}))
            .await?;
        let draft = repo
            .update_draft(&editor, id, 1, &spec("Editor instruction"), &json!({}))
            .await?;
        assert_eq!(draft["revision"], 2);
        let copied = repo.copy(&owner, id, None, None).await?;
        assert_eq!(copied["name"], "Test Copy");
        assert!(copied["slug"].as_str().unwrap().starts_with("test-copy-"));
        assert_eq!(copied["draft_revision"], 1);
        let copied_id = copied["agent_id"].as_str().unwrap();
        let copied_draft = repo.draft(&owner, copied_id).await?;
        assert_eq!(copied_draft["spec"]["capabilities"], json!([]));
        assert_eq!(copied_draft["spec"]["knowledge"], json!([]));
        let copy_audit: serde_json::Value = sqlx::query_scalar("SELECT request_summary FROM audit_logs WHERE tenant_id=$1 AND resource_id=$2 AND action='copy'")
            .bind(&owner.tenant_id).bind(copied_id).fetch_one(&p).await?;
        assert_eq!(copy_audit["source_agent_id"], id);
        error(
            repo.create_version(&editor, id, 2).await,
            404,
            "AGENT_NOT_FOUND",
        );
        error(
            repo.archive(&editor, id, false).await,
            404,
            "AGENT_NOT_FOUND",
        );
        error(
            repo.upsert_member(&editor, id, "user", "another-user", "viewer")
                .await,
            404,
            "AGENT_NOT_FOUND",
        );
        Ok(())
    })
    .await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL contract"]
async fn owner_acl_and_last_owner_protection() -> Result<()> {
    fixture(|p, repo| async move {
        let owner = actor("owner-a", "tenant-a");
        for u in ["owner-a", "owner-b"] {
            user(&p, u, "tenant-a").await?;
        }
        let a = repo
            .create(&owner, "Test", None, "", &spec("Initial"))
            .await?;
        let id = a["agent_id"].as_str().unwrap();
        error(
            repo.upsert_member(&owner, id, "user", "owner-a", "editor")
                .await,
            409,
            "AGENT_LAST_OWNER",
        );
        error(
            repo.remove_member(&owner, id, "user", "owner-a").await,
            409,
            "AGENT_LAST_OWNER",
        );
        repo.upsert_member(&owner, id, "user", "owner-b", "owner")
            .await?;
        repo.remove_member(&owner, id, "user", "owner-a").await?;
        let b = actor("owner-b", "tenant-a");
        let members = repo.members(&b, id).await?;
        assert_eq!(members.len(), 1);
        assert_eq!(members[0]["principal_id"], "owner-b");
        assert_eq!(repo.get(&b, id).await?["owner_id"], "owner-b");
        Ok(())
    })
    .await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL contract"]
async fn cross_tenant_paths_lists_and_admins_do_not_leak() -> Result<()> {
    fixture(|p, repo| async move {
        let owner = actor("owner-a", "tenant-a");
        for (u, t) in [
            ("owner-a", "tenant-a"),
            ("owner-b", "tenant-b"),
            ("admin-a", "tenant-a"),
            ("admin-b", "tenant-b"),
        ] {
            user(&p, u, t).await?;
        }
        let a = repo
            .create(&owner, "Test", None, "", &spec("Initial"))
            .await?;
        let id = a["agent_id"].as_str().unwrap();
        for mut other in [actor("owner-b", "tenant-b"), actor("admin-b", "tenant-b")] {
            if other.user_id == "admin-b" {
                other.roles = vec!["admin".into()];
            }
            assert_eq!(
                repo.list(
                    &other,
                    ListAgents {
                        limit: 50,
                        ..Default::default()
                    }
                )
                .await?,
                json!({"items":[],"next_cursor":null})
            );
            error(repo.get(&other, id).await, 404, "AGENT_NOT_FOUND");
            error(repo.draft(&other, id).await, 404, "AGENT_NOT_FOUND");
            error(
                repo.update(&other, id, &json!({"name":"Cross tenant"}))
                    .await,
                404,
                "AGENT_NOT_FOUND",
            );
            error(
                repo.update_draft(&other, id, 1, &spec("Cross tenant"), &json!({}))
                    .await,
                404,
                "AGENT_NOT_FOUND",
            );
            error(
                repo.create_version(&other, id, 1).await,
                404,
                "AGENT_NOT_FOUND",
            );
            error(repo.soft_delete(&other, id).await, 404, "AGENT_NOT_FOUND");
        }
        let mut admin = actor("admin-a", "tenant-a");
        admin.roles = vec!["admin".into()];
        repo.get(&admin, id).await?;
        repo.update(&admin, id, &json!({"description":"Admin edit"}))
            .await?;
        Ok(())
    })
    .await
}
