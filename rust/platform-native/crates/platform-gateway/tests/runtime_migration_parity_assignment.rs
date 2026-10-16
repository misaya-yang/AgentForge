//! Production session-runtime assignment lifecycle, replacing Python fake-database
//! assertions with the real AgentThreadStore and canonical migration 089.
//! Opt in explicitly with `-- --ignored`; no database calls during ordinary tests.
#[path = "agent_studio_support/mod.rs"]
mod support;
use ai_platform_gateway::migration_agent::{Scope, threads::AgentThreadStore};
use futures_util::FutureExt;
use sqlx::PgPool;
use support::{Fixture, Result};

async fn fixture<F, Fut>(test: F) -> Result<()>
where
    F: FnOnce(PgPool, AgentThreadStore) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let fixture = Fixture::custom(
        include_str!("../../platform-migration-authority/tests/fixtures/runtime_thread_store_base.sql"),
        &["089_agent_runtime_thread_store.sql"],
    ).await?;
    let pool = fixture.pool.clone();
    let store = AgentThreadStore::new(pool.clone());
    let result = std::panic::AssertUnwindSafe(test(pool, store)).catch_unwind().await;
    fixture.close().await?;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}
async fn session(pool: &PgPool, tenant: &str, user: &str) -> Result<Scope> {
    let scope = Scope { tenant_id: tenant.into(), user_id: user.into(), session_id: format!("assignment-{}", uuid::Uuid::new_v4()) };
    sqlx::query("INSERT INTO sessions(session_id,tenant_id,user_id,service_id) VALUES($1,$2,$3,'__builtin_assistant__')")
        .bind(&scope.session_id).bind(&scope.tenant_id).bind(&scope.user_id).execute(pool).await?;
    Ok(scope)
}

#[tokio::test]
#[ignore = "requires explicit isolated local PostgreSQL fixture; run with --ignored"]
async fn source_runtime_assignment_is_idempotent_and_scope_bound() -> Result<()> {
    fixture(|pool, store| async move {
        let scope = session(&pool, "tenant-a", "user-a").await?;
        let missing = store.require_assignment(&scope).await.unwrap_err();
        assert_eq!(missing.status, 404);
        assert_eq!(missing.detail["code"], "AGENT_RUNTIME_ASSIGNMENT_NOT_FOUND");
        let first = store.bind_assignment(&scope, None, "default_policy").await?;
        let replay = store.bind_assignment(&scope, None, "default_policy").await?;
        assert_eq!(first, replay);
        assert_eq!(store.require_assignment(&scope).await?, first);
        assert_eq!(first["runtime_owner"], "agent_runtime");
        for other in [
            Scope { tenant_id: "tenant-b".into(), ..scope.clone() },
            Scope { user_id: "user-b".into(), ..scope.clone() },
            Scope { session_id: "other-session".into(), ..scope.clone() },
        ] {
            assert!(store.assignment(&other).await?.is_none());
            assert_eq!(store.require_assignment(&other).await.unwrap_err().status, 404);
        }
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM assistant_session_runtime_assignments").fetch_one(&pool).await?;
        assert_eq!(count, 1);
        Ok(())
    }).await
}

#[tokio::test]
#[ignore = "requires explicit isolated local PostgreSQL fixture; run with --ignored"]
async fn source_new_session_single_kernel_is_stable_and_existing_assignment_wins() -> Result<()> {
    fixture(|pool, store| async move {
        let first_scope = session(&pool, "tenant-a", "user-a").await?;
        let other_scope = session(&pool, "other", "other").await?;
        let first = store.bind_new_assignment(&first_scope, "kernel-1").await?;
        let replay = store.bind_new_assignment(&first_scope, "kernel-2").await?;
        assert_eq!(first, replay);
        assert_eq!(replay["kernel_revision"], "kernel-1");
        for row in [first, store.bind_new_assignment(&other_scope, "kernel-1").await?] {
            assert_eq!(row["runtime_owner"], "agent_runtime");
            assert_eq!(row["assignment_reason"], "single_kernel");
        }
        // Explicit binding cannot rewrite the frozen assignment; it differs
        // from bind_new's intentionally sticky selection semantics.
        let conflict = store.bind_assignment(&first_scope, Some("kernel-2"), "single_kernel").await.unwrap_err();
        assert_eq!(conflict.status, 409);
        assert_eq!(conflict.detail["code"], "AGENT_RUNTIME_ASSIGNMENT_CONFLICT");
        assert_eq!(store.require_assignment(&first_scope).await?["kernel_revision"], "kernel-1");
        Ok(())
    }).await
}

#[tokio::test]
#[ignore = "requires explicit isolated local PostgreSQL fixture; run with --ignored"]
async fn concurrent_same_scope_binding_is_one_immutable_assignment() -> Result<()> {
    fixture(|pool, store| async move {
        let scope = session(&pool, "tenant-race", "user-race").await?;
        let (left, right) = tokio::join!(
            store.bind_assignment(&scope, Some("kernel-race"), "single_kernel"),
            store.bind_assignment(&scope, Some("kernel-race"), "single_kernel"),
        );
        assert_eq!(left?, right?);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM assistant_session_runtime_assignments WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3")
            .bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).fetch_one(&pool).await?;
        assert_eq!(count, 1);
        let mutation = sqlx::query("UPDATE assistant_session_runtime_assignments SET kernel_revision='replacement' WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3")
            .bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).execute(&pool).await.unwrap_err();
        assert_eq!(mutation.as_database_error().and_then(|e| e.code()).as_deref(), Some("55000"));
        assert_eq!(store.require_assignment(&scope).await?["kernel_revision"], "kernel-race");
        Ok(())
    }).await
}
