//! Knowledge's mixed control/data pool is backend pinned (advisory leases).
//! This installs transaction-local tenant settings only from verified ownership;
//! it never changes SQL roles and is not an RLS-policy activation mechanism.
use ai_platform_service_primitives::{
    data_connection::{
        DatabaseRole, TenantScope, bind_transaction, current_tenant_scope, with_tenant_scope,
    },
    knowledge_model::verified_actor_scope,
};
use sqlx::{Postgres, Transaction};
use std::sync::OnceLock;
mod executor;
pub use executor::{TenantExecutor, connection, executor};
static POLICY: OnceLock<(DatabaseRole, u32)> = OnceLock::new();
tokio::task_local! { static JOB_TENANT: TenantScope; }

/// Only worker paths that have read an authority-owned persisted job may install
/// this scope. It permits tenant-local bookkeeping without fabricating user roles.
pub(crate) async fn with_job_scope<F: std::future::Future>(
    tenant: &str,
    future: F,
) -> Result<F::Output, sqlx::Error> {
    let tenant = TenantScope::new(tenant)
        .map_err(|_| sqlx::Error::Protocol("invalid persisted job tenant".into()))?;
    if authenticated_scope()?.is_some_and(|existing| existing != tenant) {
        return Err(sqlx::Error::Protocol(
            "persisted job tenant context mismatch".into(),
        ));
    }
    Ok(JOB_TENANT
        .scope(tenant.clone(), with_tenant_scope(tenant, future))
        .await)
}

/// Global queue discovery has a narrow authority-owned entrypoint after RLS
/// activation; tenant requests keep their transaction-local scope inside it.
pub async fn control_enabled(pool: &sqlx::PgPool, function: &str) -> Result<bool, sqlx::Error> {
    let (enabled, callable): (bool, bool) = sqlx::query_as(
        "SELECT c.relrowsecurity, COALESCE(pg_catalog.has_function_privilege(\
             current_user,pg_catalog.to_regprocedure($1),'EXECUTE'),false) \
         FROM pg_catalog.pg_class c WHERE c.oid='knowledge.datasets'::regclass",
    )
    .bind(function)
    .fetch_one(pool)
    .await?;
    if enabled && !callable {
        return Err(sqlx::Error::Protocol(
            "Knowledge RLS control entrypoint missing".into(),
        ));
    }
    Ok(enabled)
}
pub fn configure(role: DatabaseRole, timeout_ms: u32) -> Result<(), &'static str> {
    if !matches!(
        role,
        DatabaseRole::KnowledgeApi | DatabaseRole::KnowledgeWorker
    ) || !(100..=300000).contains(&timeout_ms)
    {
        return Err("knowledge_database_policy");
    }
    let policy = (role, timeout_ms);
    if let Some(previous) = POLICY.get() {
        return if *previous == policy {
            Ok(())
        } else {
            Err("knowledge_database_policy_conflict")
        };
    }
    POLICY
        .set(policy)
        .map_err(|_| "knowledge_database_policy_conflict")
}
fn authenticated_scope() -> Result<Option<TenantScope>, sqlx::Error> {
    let actor = verified_actor_scope();
    let scope = current_tenant_scope().ok();
    match actor {
        Some(actor) => {
            if actor.anonymous() {
                return Err(sqlx::Error::Protocol(
                    "unverified Knowledge database actor".into(),
                ));
            }
            let tenant = TenantScope::new(&actor.tenant_id)
                .map_err(|_| sqlx::Error::Protocol("invalid Knowledge database tenant".into()))?;
            if scope.as_ref().is_some_and(|scope| scope != &tenant) {
                return Err(sqlx::Error::Protocol(
                    "Knowledge database tenant context mismatch".into(),
                ));
            }
            Ok(Some(tenant))
        }
        // No task-local tenant can manufacture a user actor. Startup/queue scans
        // remain existing role-limited control operations on direct/session pools.
        None => match (scope, JOB_TENANT.try_with(Clone::clone).ok()) {
            (Some(scope), Some(job)) if scope == job => Ok(Some(scope)),
            (None, None) => Ok(None),
            _ => Err(sqlx::Error::Protocol(
                "Knowledge database tenant requires verified ownership".into(),
            )),
        },
    }
}
/// Applied immediately after BEGIN, before every explicit business transaction.
/// Global discovery has no fabricated tenant; this is why the mixed pool refuses
/// transaction pooling at construction and no RLS proposal is activated here.
pub async fn bind_current(transaction: &mut Transaction<'_, Postgres>) -> Result<(), sqlx::Error> {
    if let Some(scope) = authenticated_scope()? {
        let (role, timeout) = POLICY
            .get()
            .copied()
            .unwrap_or((DatabaseRole::KnowledgeApi, 30000));
        bind_transaction(transaction, &scope, role, timeout)
            .await
            .map_err(|_| {
                sqlx::Error::Protocol("Knowledge tenant transaction binding failed".into())
            })?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use ai_platform_service_primitives::data_connection::with_tenant_scope;
    #[tokio::test]
    async fn persisted_jobs_restore_context_and_cannot_cross_actor_tenants() {
        with_job_scope("tenant-a", async {
            assert_eq!(authenticated_scope().unwrap().unwrap().as_str(), "tenant-a");
            assert!(with_job_scope("tenant-b", async {}).await.is_err());
            let actor = serde_json::from_value(
                serde_json::json!({"tenant_id":"tenant-b","user_id":"user"}),
            )
            .unwrap();
            ai_platform_service_primitives::knowledge_model::with_actor_scope(actor, async {
                assert!(authenticated_scope().is_err());
            })
            .await;
            assert_eq!(authenticated_scope().unwrap().unwrap().as_str(), "tenant-a");
            assert!(
                tokio::spawn(async { authenticated_scope().unwrap().is_none() })
                    .await
                    .unwrap()
            );
        })
        .await
        .unwrap();
        assert!(authenticated_scope().unwrap().is_none());
    }
    #[tokio::test]
    async fn task_scope_without_verified_actor_is_rejected() {
        with_tenant_scope(TenantScope::new("tenant").unwrap(), async {
            assert!(authenticated_scope().is_err());
        })
        .await;
    }
    #[tokio::test]
    async fn control_scope_is_not_an_empty_tenant() {
        assert_eq!(authenticated_scope().unwrap(), None);
    }
    #[tokio::test]
    async fn authenticated_actor_binds_only_its_own_tenant() {
        use ai_platform_service_primitives::knowledge_model::with_actor_scope;
        let actor =
            serde_json::from_value(serde_json::json!({"tenant_id":"t1","user_id":"u1"})).unwrap();
        with_actor_scope(actor, async {
            assert_eq!(authenticated_scope().unwrap().unwrap().as_str(), "t1");
            with_tenant_scope(TenantScope::new("t2").unwrap(), async {
                assert!(authenticated_scope().is_err());
            })
            .await;
            assert_eq!(authenticated_scope().unwrap().unwrap().as_str(), "t1");
        })
        .await;
        assert!(authenticated_scope().unwrap().is_none());
    }
    #[tokio::test]
    async fn spawned_task_does_not_inherit_actor_or_tenant() {
        use ai_platform_service_primitives::knowledge_model::with_actor_scope;
        let actor =
            serde_json::from_value(serde_json::json!({"tenant_id":"t1","user_id":"u1"})).unwrap();
        with_actor_scope(
            actor,
            with_tenant_scope(TenantScope::new("t1").unwrap(), async {
                assert!(authenticated_scope().unwrap().is_some());
                assert!(
                    tokio::spawn(async { authenticated_scope().unwrap().is_none() })
                        .await
                        .unwrap()
                );
            }),
        )
        .await;
    }
}
