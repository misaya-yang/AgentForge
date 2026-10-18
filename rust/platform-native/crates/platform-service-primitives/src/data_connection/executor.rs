//! Transaction-scoped SQLx execution for verified tenant queries and pinned leases.
use super::{DatabaseRole, TenantScope, bind_transaction, validate_tenant_binding};
use futures_util::{TryStreamExt, future::BoxFuture, stream::BoxStream};
use sqlx::{
    Connection, Describe, Either, Error, Execute, Executor, PgConnection, PgPool, Postgres,
    postgres::{PgQueryResult, PgRow, PgStatement, PgTypeInfo},
};

pub struct TenantExecutor<'p> {
    target: Target<'p>,
    tenant: Box<dyn FnOnce() -> Result<Option<TenantScope>, Error> + Send + 'p>,
    role: DatabaseRole,
    timeout: u32,
}
enum Target<'p> {
    Pool(&'p PgPool),
    Connection(&'p mut PgConnection),
}
impl std::fmt::Debug for TenantExecutor<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TenantExecutor").finish_non_exhaustive()
    }
}
impl<'p> TenantExecutor<'p> {
    /// Resolvers may return None only for existing role-limited control queries.
    pub fn pool(
        pool: &'p PgPool,
        role: DatabaseRole,
        timeout: u32,
        resolver: impl FnOnce() -> Result<Option<TenantScope>, Error> + Send + 'p,
    ) -> Self {
        Self {
            target: Target::Pool(pool),
            tenant: Box::new(resolver),
            role,
            timeout,
        }
    }
    pub fn connection(
        connection: &'p mut PgConnection,
        role: DatabaseRole,
        timeout: u32,
        resolver: impl FnOnce() -> Result<Option<TenantScope>, Error> + Send + 'p,
    ) -> Self {
        Self {
            target: Target::Connection(connection),
            tenant: Box::new(resolver),
            role,
            timeout,
        }
    }
}
/// Ownership must come from an authenticated actor or an authority-verified job,
/// never directly from a request body. Validate at capture and again at execution.
pub fn tenant_executor<'p>(
    pool: &'p PgPool,
    tenant: &str,
    role: DatabaseRole,
    timeout: u32,
) -> TenantExecutor<'p> {
    let captured = TenantScope::new(tenant).and_then(|scope| {
        validate_tenant_binding(&scope)?;
        Ok(scope)
    });
    TenantExecutor::pool(pool, role, timeout, move || {
        let scope = captured.map_err(|_| Error::Protocol("tenant scope invalid".into()))?;
        validate_tenant_binding(&scope)
            .map_err(|_| Error::Protocol("tenant scope changed".into()))?;
        Ok(Some(scope))
    })
}

async fn begin<'p>(
    target: Target<'p>,
    tenant: &TenantScope,
    role: DatabaseRole,
    timeout: u32,
) -> Result<sqlx::Transaction<'p, Postgres>, Error> {
    let mut tx = match target {
        Target::Pool(pool) => pool.begin().await?,
        Target::Connection(connection) => connection.begin().await?,
    };
    bind_transaction(&mut tx, tenant, role, timeout)
        .await
        .map_err(|_| Error::Protocol("tenant transaction binding failed".into()))?;
    Ok(tx)
}

impl<'c> Executor<'c> for TenantExecutor<'c> {
    type Database = Postgres;

    fn fetch_many<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> BoxStream<'e, Result<Either<PgQueryResult, PgRow>, Error>>
    where
        'c: 'e,
        E: 'q + Execute<'q, Postgres>,
    {
        Box::pin(async_stream::try_stream! {
            match (self.tenant)()? {
                Some(tenant) => {
                    let mut tx = begin(self.target, &tenant, self.role, self.timeout).await?;
                    {
                        let mut rows = (&mut *tx).fetch_many(query);
                        while let Some(row) = rows.try_next().await? {
                            yield row;
                        }
                    }
                    tx.commit().await?;
                }
                None => {
                    let mut rows = match self.target {
                        Target::Pool(pool) => pool.fetch_many(query),
                        Target::Connection(connection) => connection.fetch_many(query),
                    };
                    while let Some(row) = rows.try_next().await? {
                        yield row;
                    }
                }
            }
        })
    }

    fn fetch_optional<'e, 'q: 'e, E>(self, query: E) -> BoxFuture<'e, Result<Option<PgRow>, Error>>
    where
        'c: 'e,
        E: 'q + Execute<'q, Postgres>,
    {
        Box::pin(async move {
            match (self.tenant)()? {
                Some(tenant) => {
                    let mut tx = begin(self.target, &tenant, self.role, self.timeout).await?;
                    let row = (&mut *tx).fetch_optional(query).await?;
                    tx.commit().await?;
                    Ok(row)
                }
                None => match self.target {
                    Target::Pool(pool) => pool.fetch_optional(query).await,
                    Target::Connection(connection) => connection.fetch_optional(query).await,
                },
            }
        })
    }

    fn prepare_with<'e, 'q: 'e>(
        self,
        sql: &'q str,
        parameters: &'e [PgTypeInfo],
    ) -> BoxFuture<'e, Result<PgStatement<'q>, Error>>
    where
        'c: 'e,
    {
        Box::pin(async move {
            (self.tenant)()?;
            match self.target {
                Target::Pool(pool) => pool.prepare_with(sql, parameters).await,
                Target::Connection(connection) => connection.prepare_with(sql, parameters).await,
            }
        })
    }

    fn describe<'e, 'q: 'e>(self, sql: &'q str) -> BoxFuture<'e, Result<Describe<Postgres>, Error>>
    where
        'c: 'e,
    {
        Box::pin(async move {
            (self.tenant)()?;
            match self.target {
                Target::Pool(pool) => pool.describe(sql).await,
                Target::Connection(connection) => connection.describe(sql).await,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_connection::with_tenant_scope;

    #[tokio::test]
    async fn invalid_and_cross_tenant_queries_fail_before_connecting() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://synthetic:synthetic@127.0.0.1:1/unused")
            .unwrap();
        let invalid = sqlx::query("SELECT 1")
            .fetch_optional(tenant_executor(&pool, "", DatabaseRole::Gateway, 1000))
            .await
            .unwrap_err();
        assert!(matches!(invalid, Error::Protocol(_)));
        with_tenant_scope(TenantScope::new("tenant-a").unwrap(), async {
            let foreign = tenant_executor(&pool, "tenant-b", DatabaseRole::Gateway, 1000);
            let error = sqlx::query("SELECT 1")
                .fetch_optional(foreign)
                .await
                .unwrap_err();
            assert!(matches!(error, Error::Protocol(_)));
        })
        .await;
        let captured = tenant_executor(&pool, "tenant-a", DatabaseRole::Gateway, 1000);
        with_tenant_scope(TenantScope::new("tenant-b").unwrap(), async {
            let error = sqlx::query("SELECT 1")
                .fetch_optional(captured)
                .await
                .unwrap_err();
            assert!(matches!(error, Error::Protocol(_)));
        })
        .await;
        pool.close().await;
    }
}
