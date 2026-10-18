//! Bind Knowledge queries to verified request or persisted worker ownership.
use super::{POLICY, authenticated_scope};
pub use ai_platform_service_primitives::data_connection::TenantExecutor;
use ai_platform_service_primitives::data_connection::{DatabaseRole, TenantScope};
use sqlx::{Error, PgConnection, PgPool};

pub fn executor(pool: &PgPool) -> TenantExecutor<'_> {
    let captured = authenticated_scope();
    let (role, timeout) = POLICY
        .get()
        .copied()
        .unwrap_or((DatabaseRole::KnowledgeApi, 30000));
    TenantExecutor::pool(pool, role, timeout, move || resolve_scope(captured))
}
pub fn connection(connection: &mut PgConnection) -> TenantExecutor<'_> {
    let captured = authenticated_scope();
    let (role, timeout) = POLICY
        .get()
        .copied()
        .unwrap_or((DatabaseRole::KnowledgeApi, 30000));
    TenantExecutor::connection(connection, role, timeout, move || resolve_scope(captured))
}

fn resolve_scope(
    captured: Result<Option<TenantScope>, Error>,
) -> Result<Option<TenantScope>, Error> {
    let captured = captured?;
    let current = authenticated_scope()?;
    if captured
        .as_ref()
        .zip(current.as_ref())
        .is_some_and(|(a, b)| a != b)
    {
        return Err(Error::Protocol(
            "Knowledge query moved to a different tenant".into(),
        ));
    }
    Ok(captured.or(current))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_platform_service_primitives::{
        knowledge_model::with_actor_scope, knowledge_types::UserContext,
    };

    fn actor(tenant: &str) -> UserContext {
        UserContext {
            tenant_id: tenant.into(),
            user_id: "user".into(),
            roles: vec![],
            user_tier: "normal".into(),
            user_type: "user".into(),
            is_authenticated: true,
        }
    }

    #[tokio::test]
    async fn captured_request_scope_survives_stream_handoff_but_cannot_cross_tenants() {
        let scope = with_actor_scope(actor("tenant-a"), async { authenticated_scope() }).await;
        let retained = resolve_scope(scope).unwrap().unwrap();
        assert_eq!(retained.as_str(), "tenant-a");
        with_actor_scope(actor("tenant-b"), async {
            assert!(resolve_scope(Ok(Some(retained))).is_err());
            assert_eq!(
                resolve_scope(Ok(None)).unwrap().unwrap().as_str(),
                "tenant-b"
            );
        })
        .await;
        assert!(resolve_scope(Ok(None)).unwrap().is_none());
    }

    #[tokio::test]
    async fn invalid_actor_is_rejected_before_a_database_connection_is_acquired() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://synthetic:synthetic@127.0.0.1:1/unused")
            .unwrap();
        let mut anonymous = actor("tenant-a");
        anonymous.is_authenticated = false;
        with_actor_scope(anonymous, async {
            let error = sqlx::query("SELECT 1")
                .fetch_optional(executor(&pool))
                .await
                .unwrap_err();
            assert!(matches!(error, Error::Protocol(_)), "{error}");
        })
        .await;
        pool.close().await;
    }
}
