//! Native append writer switch. Call inside the existing business transaction.
//! Authority admission is per family. Query errors never select the legacy path.
mod sql;
use serde_json::Value;
pub use sql::{Statement, parse_statement};
use sqlx::{
    Decode, Execute, FromRow, PgConnection, Postgres, Row, Type,
    postgres::{PgArguments, PgQueryResult, PgRow},
};

fn contract(message: &str) -> sqlx::Error {
    sqlx::Error::Protocol(message.into())
}
/// The installed authority function holds a family shared transaction lock;
/// expansion takes its exclusive counterpart before installing global guards.
/// An absent successor is the only pre-admission legacy result.
pub async fn enabled(connection: &mut PgConnection, family: &str) -> Result<bool, sqlx::Error> {
    if !super::append::FAMILIES.contains(&family) {
        return Err(contract("unknown append family"));
    }
    let installed: bool = sqlx::query_scalar(
        "SELECT to_regprocedure('partition_ops.family_enabled(text)') IS NOT NULL",
    )
    .fetch_one(&mut *connection)
    .await?;
    if !installed {
        return Ok(false);
    }
    sqlx::query_scalar("SELECT partition_ops.family_enabled($1)")
        .bind(family)
        .fetch_one(connection)
        .await
}
async fn prepare<'q>(
    connection: &mut PgConnection,
    family: &str,
    mut query: impl Execute<'q, Postgres>,
) -> Result<(String, PgArguments), sqlx::Error> {
    let original = query.sql().to_owned();
    let arguments = query.take_arguments().map_err(sqlx::Error::Encode)?;
    if !enabled(connection, family).await? {
        return Ok((original, arguments.unwrap_or_default()));
    }
    let statement = parse_statement(family, &original).map_err(contract)?;
    let types: Value = sqlx::query_scalar("SELECT partition_ops.column_types($1)")
        .bind(family)
        .fetch_one(&mut *connection)
        .await?;
    Ok((
        statement.rewrite(&types).map_err(contract)?,
        arguments.unwrap_or_default(),
    ))
}
pub async fn execute<'q>(
    connection: &mut PgConnection,
    family: &str,
    query: impl Execute<'q, Postgres>,
) -> Result<PgQueryResult, sqlx::Error> {
    let (sql, args) = prepare(connection, family, query).await?;
    sqlx::query_with(&sql, args).execute(connection).await
}
pub async fn fetch_optional<'q>(
    connection: &mut PgConnection,
    family: &str,
    query: impl Execute<'q, Postgres>,
) -> Result<Option<PgRow>, sqlx::Error> {
    let (sql, args) = prepare(connection, family, query).await?;
    sqlx::query_with(&sql, args)
        .fetch_optional(connection)
        .await
}
pub async fn fetch_scalar_optional<'q, T>(
    connection: &mut PgConnection,
    family: &str,
    query: impl Execute<'q, Postgres>,
) -> Result<Option<T>, sqlx::Error>
where
    T: for<'r> Decode<'r, Postgres> + Type<Postgres>,
{
    fetch_optional(connection, family, query)
        .await?
        .map(|row| row.try_get(0))
        .transpose()
}
pub async fn fetch_all<'q, O>(
    connection: &mut PgConnection,
    family: &str,
    query: impl Execute<'q, Postgres>,
) -> Result<Vec<O>, sqlx::Error>
where
    O: for<'r> FromRow<'r, PgRow>,
{
    let (sql, args) = prepare(connection, family, query).await?;
    sqlx::query_with(&sql, args)
        .fetch_all(connection)
        .await?
        .iter()
        .map(O::from_row)
        .collect()
}
/// Nullable timestamp families route NULL into MINVALUE. Keep the original
/// predicate (and thus NULL/date semantics), add an equivalent pruning bound.
/// Callers use this only for controlled compiled SQL, never request text.
pub async fn pruning_sql(
    connection: &mut PgConnection,
    family: &str,
    original: &str,
) -> Result<String, sqlx::Error> {
    if !enabled(connection, family).await? {
        return Ok(original.to_owned());
    }
    if !matches!(family, "gateway.usage_records" | "gateway.tool_audit_log") {
        return Ok(original.to_owned());
    }
    sql::pruning(original).map_err(contract)
}

/// Bind only a tenant already established by the business owner. Background
/// batches explicitly iterate their verified tenant records in one transaction.
/// A request task's authenticated scope can never be replaced by a record field.
pub async fn bind_tenant(
    connection: &mut PgConnection,
    family: &str,
    verified: &str,
) -> Result<(), sqlx::Error> {
    if !enabled(connection, family).await? {
        return Ok(());
    }
    let scope = super::TenantScope::new(verified).map_err(|_| contract("invalid append tenant"))?;
    if super::current_tenant_scope().is_ok_and(|current| current != scope) {
        return Err(contract("append request tenant mismatch"));
    }
    sqlx::query("SELECT pg_catalog.set_config('app.tenant_id',$1,true)")
        .bind(scope.as_str())
        .execute(connection)
        .await?;
    Ok(())
}
/// Read a controlled QueryBuilder/query through the nullable-clock projection,
/// retaining all original SQLx binds, decoders and caller transaction.
pub async fn read_all<'q, O>(
    connection: &mut PgConnection,
    family: &str,
    mut query: impl Execute<'q, Postgres>,
) -> Result<Vec<O>, sqlx::Error>
where
    O: for<'r> FromRow<'r, PgRow>,
{
    let sql = pruning_sql(connection, family, query.sql()).await?;
    let args = query
        .take_arguments()
        .map_err(sqlx::Error::Encode)?
        .unwrap_or_default();
    sqlx::query_with(&sql, args)
        .fetch_all(connection)
        .await?
        .iter()
        .map(O::from_row)
        .collect()
}
pub async fn read_one<'q, O>(
    connection: &mut PgConnection,
    family: &str,
    query: impl Execute<'q, Postgres>,
) -> Result<O, sqlx::Error>
where
    O: for<'r> FromRow<'r, PgRow>,
{
    let mut rows = read_all(connection, family, query).await?;
    if rows.len() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(rows.remove(0))
}
