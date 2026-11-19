//! Database context from verified Gateway actors and persisted job ownership.
use ai_platform_service_primitives::data_connection::{
    DatabaseRole, TenantExecutor, TenantScope, bind_transaction, tenant_executor,
    validate_tenant_binding,
};
use sqlx::{Error, PgConnection, PgPool, Postgres, Transaction};

pub fn executor<'p>(pool: &'p PgPool, tenant: &str) -> TenantExecutor<'p> {
    tenant_executor(pool, tenant, DatabaseRole::Gateway, 30_000)
}

pub fn connection<'p>(connection: &'p mut PgConnection, tenant: &str) -> TenantExecutor<'p> {
    let captured = TenantScope::new(tenant).and_then(|scope| {
        validate_tenant_binding(&scope)?;
        Ok(scope)
    });
    TenantExecutor::connection(connection, DatabaseRole::Gateway, 30_000, move || {
        let scope = captured.map_err(|_| Error::Protocol("invalid Gateway tenant".into()))?;
        validate_tenant_binding(&scope)
            .map_err(|_| Error::Protocol("Gateway tenant changed".into()))?;
        Ok(Some(scope))
    })
}

pub async fn begin(pool: &PgPool, tenant: &str) -> Result<Transaction<'static, Postgres>, Error> {
    let scope = TenantScope::new(tenant)
        .map_err(|_| Error::Protocol("invalid Gateway database tenant".into()))?;
    validate_tenant_binding(&scope)
        .map_err(|_| Error::Protocol("Gateway database tenant mismatch".into()))?;
    let mut transaction = pool.begin().await?;
    bind_transaction(&mut transaction, &scope, DatabaseRole::Gateway, 30_000)
        .await
        .map_err(|_| Error::Protocol("Gateway tenant transaction binding failed".into()))?;
    Ok(transaction)
}

/// A run ID is accepted only from an existing control-plane operation. The
/// authority exposes ownership metadata so background completion can bind RLS.
pub async fn run_tenant(pool: &PgPool, run: &str) -> Result<String, Error> {
    ownership_tenant(
        pool,
        run,
        "assistant.assistant_runs",
        "assistant.gateway_run_scope_v1(uuid)",
        "SELECT tenant_id FROM assistant.gateway_run_scope_v1($1::uuid)",
        "SELECT tenant_id::text FROM assistant.assistant_runs WHERE run_id=$1::uuid",
    )
    .await
}

pub async fn model_lease_tenant(pool: &PgPool, lease: &str) -> Result<String, Error> {
    ownership_tenant(pool, lease, "assistant.assistant_runtime_model_leases", "assistant.gateway_model_lease_scope_v1(uuid)",
        "SELECT tenant_id FROM assistant.gateway_model_lease_scope_v1($1::uuid)",
        "SELECT tenant_id::text FROM assistant.assistant_runtime_model_leases WHERE lease_id=$1::uuid").await
}

pub async fn model_call_tenant(pool: &PgPool, call: &str) -> Result<String, Error> {
    ownership_tenant(pool, call, "assistant.assistant_runtime_model_calls", "assistant.gateway_model_call_scope_v1(uuid)",
        "SELECT tenant_id FROM assistant.gateway_model_call_scope_v1($1::uuid)",
        "SELECT tenant_id::text FROM assistant.assistant_runtime_model_calls WHERE call_id=$1::uuid").await
}

async fn ownership_tenant(
    pool: &PgPool,
    id: &str,
    table: &str,
    function: &str,
    control_sql: &str,
    scoped_sql: &str,
) -> Result<String, Error> {
    let (enabled, callable): (bool, bool) = sqlx::query_as(
        "SELECT c.relrowsecurity,COALESCE(pg_catalog.has_function_privilege(current_user,pg_catalog.to_regprocedure($2),'EXECUTE'),false) FROM pg_catalog.pg_class c WHERE c.oid=pg_catalog.to_regclass($1)",
    ).bind(table).bind(function).fetch_one(pool).await?;
    if enabled && !callable {
        return Err(Error::Protocol(
            "Gateway RLS ownership entrypoint missing".into(),
        ));
    }
    let tenant: String = sqlx::query_scalar(if enabled { control_sql } else { scoped_sql })
        .bind(id)
        .fetch_one(pool)
        .await?;
    let scope = TenantScope::new(&tenant)
        .map_err(|_| Error::Protocol("invalid persisted tenant".into()))?;
    validate_tenant_binding(&scope)
        .map_err(|_| Error::Protocol("persisted tenant scope mismatch".into()))?;
    Ok(tenant)
}
