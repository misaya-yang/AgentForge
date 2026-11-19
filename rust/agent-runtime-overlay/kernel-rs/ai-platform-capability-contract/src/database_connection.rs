//! Platform-owned file DSN adapter. Reuses the native data authority's exact
//! role/physical-database checks and never prints or exports secret material.
#[path = "shared_data_connection/config.rs"]
pub mod policy;
use policy::{ConnectionConfig, ConnectionPurpose, DataError, DatabaseRole};
use sqlx::{PgPool, Postgres, Transaction, postgres::PgConnectOptions};
use std::collections::BTreeMap;

/// Bind a transaction to the admitted platform scope without changing its role.
/// Reusing a transaction for a different tenant fails before any setting changes.
pub async fn bind_tenant(
    transaction: &mut Transaction<'_, Postgres>,
    tenant: &str,
    role: DatabaseRole,
) -> Result<(), sqlx::Error> {
    if tenant.trim().is_empty() || tenant.len() > 255 || tenant.contains('\0') {
        return Err(sqlx::Error::Protocol(
            "invalid platform database tenant".into(),
        ));
    }
    let installed: Option<String> = sqlx::query_scalar(
        "SELECT pg_catalog.set_config('app.tenant_id',$1,true), \
         pg_catalog.set_config('row_security','on',true) \
         WHERE current_user::text=$2 \
         AND (NULLIF(pg_catalog.current_setting('app.tenant_id',true),'') IS NULL \
              OR pg_catalog.current_setting('app.tenant_id',true)=$1)",
    )
    .bind(tenant)
    .bind(role.name())
    .fetch_optional(&mut **transaction)
    .await?;
    if installed.as_deref() != Some(tenant) {
        return Err(sqlx::Error::Protocol(
            "platform database tenant or role mismatch".into(),
        ));
    }
    Ok(())
}

/// RLS activation must install the matching bounded service-control entrypoint.
/// A missing entrypoint is a schema error, never an unrestricted-table fallback.
pub async fn tenant_controls_enabled(
    pool: &PgPool,
    table: &str,
    function: &str,
) -> Result<bool, sqlx::Error> {
    let (enabled, callable): (bool, bool) = sqlx::query_as(
        "SELECT c.relrowsecurity, COALESCE(pg_catalog.has_function_privilege(\
            current_user,pg_catalog.to_regprocedure($2),'EXECUTE'),false) \
         FROM pg_catalog.pg_class c WHERE c.oid=pg_catalog.to_regclass($1)",
    )
    .bind(table)
    .bind(function)
    .fetch_one(pool)
    .await?;
    if enabled && !callable {
        return Err(sqlx::Error::Protocol(
            "tenant control schema is incomplete".into(),
        ));
    }
    Ok(enabled)
}

pub struct ServiceDatabase {
    config: ConnectionConfig,
    options: PgConnectOptions,
}
impl ServiceDatabase {
    pub fn from_environment(prefix: &str, role: DatabaseRole) -> Result<Self, DataError> {
        let environment = std::env::vars().collect::<BTreeMap<_, _>>();
        Self::from_values(prefix, role, &environment)
    }
    pub fn from_values(
        prefix: &str,
        role: DatabaseRole,
        environment: &BTreeMap<String, String>,
    ) -> Result<Self, DataError> {
        if ["POSTGRES_PASSWORD", "PGPASSWORD", "DATABASE_URL"]
            .iter()
            .any(|key| environment.get(*key).is_some_and(|v| !v.is_empty()))
        {
            return Err(DataError("legacy_inline_credentials_forbidden"));
        }
        let config = ConnectionConfig::from_environment(
            prefix,
            role,
            ConnectionPurpose::BackendPinned,
            environment,
        )?;
        // BackendPinned admits direct/session, never transaction pooling. This
        // preserves LISTEN, backend-PID, advisory-lock and owner-lease semantics.
        let options = config.connect_options()?.options([
            ("search_path", "pg_catalog,assistant"),
            (
                "statement_timeout",
                &config.statement_timeout_ms.to_string(),
            ),
            ("idle_in_transaction_session_timeout", "15000"),
        ]);
        Ok(Self { config, options })
    }
    pub fn options(&self) -> &PgConnectOptions {
        &self.options
    }
    pub async fn connect(&self) -> Result<PgPool, DataError> {
        let pool = self
            .config
            .pool_options()?
            .connect_with(self.options.clone())
            .await
            .map_err(|_| DataError("connect_failed"))?;
        self.config.verify_pool_identity(&pool).await?;
        Ok(pool)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_configuration_does_not_silently_retain_inline_or_transaction_authority() {
        let mut values =
            BTreeMap::from([("POSTGRES_PASSWORD".into(), "fixture-never-used".into())]);
        assert!(matches!(
            ServiceDatabase::from_values("DATA_RUNTIME", DatabaseRole::Runtime, &values),
            Err(DataError("legacy_inline_credentials_forbidden"))
        ));
        values = BTreeMap::from([
            ("DATA_RUNTIME_MODE".into(), "transaction".into()),
            (
                "DATA_RUNTIME_DSN_FILE".into(),
                "/synthetic/unread-dsn".into(),
            ),
            (
                "DATA_RUNTIME_TLS_CA_FILE".into(),
                "/synthetic/unread-ca".into(),
            ),
            (
                "DATA_RUNTIME_TLS_CERT_FILE".into(),
                "/synthetic/unread-cert".into(),
            ),
            (
                "DATA_RUNTIME_TLS_KEY_FILE".into(),
                "/synthetic/unread-key".into(),
            ),
            ("DATA_RUNTIME_EXPECTED_HOST".into(), "database".into()),
            ("DATA_RUNTIME_DATABASE_ALIAS".into(), "runtime_tx".into()),
            ("DATA_RUNTIME_PHYSICAL_DATABASE".into(), "platform".into()),
        ]);
        assert!(matches!(
            ServiceDatabase::from_values("DATA_RUNTIME", DatabaseRole::Runtime, &values),
            Err(DataError("transaction_pool_requires_scoped_operation"))
        ));
    }
}
