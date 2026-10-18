use super::config::{ConnectionConfig, ConnectionPurpose, DataError, DatabaseRole, ensure};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
tokio::task_local! { static DATABASE_TENANT: TenantScope; }
/// Install only from verified request/job ownership. Spawned tasks must explicitly
/// capture/rebind this scope; missing scope is an error, never a global principal.
pub async fn with_tenant_scope<F: std::future::Future>(scope: TenantScope, future: F) -> F::Output {
    DATABASE_TENANT.scope(scope, future).await
}
pub fn current_tenant_scope() -> Result<TenantScope, DataError> {
    DATABASE_TENANT
        .try_with(Clone::clone)
        .map_err(|_| DataError("tenant_context_missing"))
}
pub async fn bind_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    role: DatabaseRole,
    statement_timeout_ms: u32,
) -> Result<(), DataError> {
    validate_tenant_binding(scope)?;
    ensure(
        (100..=300000).contains(&statement_timeout_ms),
        "statement_timeout_bounds",
    )?;
    let bound: Option<String> = sqlx::query_scalar(TENANT_CONTEXT_SQL)
        .bind(scope.as_str())
        .bind(role.search_path())
        .bind(statement_timeout_ms.to_string())
        .bind(role.name())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|_| DataError("tenant_context_failed"))?;
    ensure(
        bound.as_deref() == Some(scope.as_str()),
        "tenant_or_role_rebind_refused",
    )
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TenantScope(String);
impl TenantScope {
    pub fn new(verified_tenant: &str) -> Result<Self, DataError> {
        ensure(
            !verified_tenant.is_empty()
                && verified_tenant.len() <= 255
                && verified_tenant
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'),
            "tenant_scope_invalid",
        )?;
        Ok(Self(verified_tenant.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Clone)]
pub struct TenantPool {
    pool: PgPool,
    role: DatabaseRole,
    statement_timeout_ms: u32,
}
/// A verified request scope cannot be replaced by a record's tenant. An explicit
/// owner-verified background scope remains supported when no request scope exists.
pub fn validate_tenant_binding(scope: &TenantScope) -> Result<(), DataError> {
    ensure(
        !current_tenant_scope().is_ok_and(|current| current != *scope),
        "request_tenant_mismatch",
    )
}
/// The WHERE gate is evaluated before volatile set_config projection: a different
/// already-bound transaction or actual login cannot be silently rebound. The
/// first selected column is the installed tenant, checked by bind_transaction.
pub const TENANT_CONTEXT_SQL: &str = "SELECT pg_catalog.set_config('app.tenant_id',$1,true), pg_catalog.set_config('search_path',$2,true), pg_catalog.set_config('statement_timeout',$3,true), pg_catalog.set_config('idle_in_transaction_session_timeout','30000',true), pg_catalog.set_config('row_security','on',true) WHERE current_user::text=$4 AND (NULLIF(pg_catalog.current_setting('app.tenant_id',true),'') IS NULL OR pg_catalog.current_setting('app.tenant_id',true)=$1)";

impl TenantPool {
    pub async fn connect(config: &ConnectionConfig) -> Result<Self, DataError> {
        ensure(
            config.purpose == ConnectionPurpose::TenantTransactions,
            "tenant_pool_purpose",
        )?;
        let pool = config.connect().await?;
        Ok(Self {
            pool,
            role: config.role,
            statement_timeout_ms: config.statement_timeout_ms,
        })
    }
    /// Existing owner-created pools may be wrapped only after their connection
    /// identity and mTLS policy have already been checked by the startup owner.
    pub fn from_verified_pool(
        pool: PgPool,
        role: DatabaseRole,
        statement_timeout_ms: u32,
    ) -> Result<Self, DataError> {
        ensure(
            (100..=300000).contains(&statement_timeout_ms),
            "statement_timeout_bounds",
        )?;
        Ok(Self {
            pool,
            role,
            statement_timeout_ms,
        })
    }
    pub async fn begin(&self, scope: &TenantScope) -> Result<TenantTransaction, DataError> {
        validate_tenant_binding(scope)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| DataError("begin_failed"))?;
        bind_transaction(
            &mut transaction,
            scope,
            self.role,
            self.statement_timeout_ms,
        )
        .await?;
        Ok(TenantTransaction {
            transaction,
            scope: scope.clone(),
        })
    }
    pub async fn close(&self) {
        self.pool.close().await;
    }
}
pub struct TenantTransaction {
    transaction: Transaction<'static, Postgres>,
    scope: TenantScope,
}
impl TenantTransaction {
    pub fn scope(&self) -> &TenantScope {
        &self.scope
    }
    pub fn connection(&mut self) -> &mut PgConnection {
        &mut self.transaction
    }
    pub async fn verify_context(&mut self) -> Result<String, DataError> {
        validate_tenant_binding(&self.scope)?;
        let actual: String =
            sqlx::query_scalar("SELECT pg_catalog.current_setting('app.tenant_id')")
                .fetch_one(&mut *self.transaction)
                .await
                .map_err(|_| DataError("tenant_context_probe_failed"))?;
        ensure(actual == self.scope.as_str(), "tenant_context_mismatch")?;
        Ok(actual)
    }
    pub async fn commit(self) -> Result<(), DataError> {
        self.transaction
            .commit()
            .await
            .map_err(|_| DataError("commit_failed"))
    }
    pub async fn rollback(self) -> Result<(), DataError> {
        self.transaction
            .rollback()
            .await
            .map_err(|_| DataError("rollback_failed"))
    }
}
// Dropping the wrapper drops SQLx Transaction: rollback is queued on the still
// checked-out connection before it can return to the pool. Tenant settings are
// transaction-local, including error, cancellation and ordinary drop paths.
