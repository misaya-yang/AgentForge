use crate::{AuthorityError, DEFAULT_ROLE_PREFIX, Result};
use regex::Regex;
use sqlx::{Connection, PgConnection, Row};
use std::{collections::BTreeMap, sync::LazyLock};
pub const APPLICATION_PRINCIPALS: &[&str] = &[
    "gateway",
    "runtime",
    "capability_worker",
    "knowledge_api",
    "knowledge_worker",
];
pub const ALL_PRINCIPALS: &[&str] = &[
    "admin",
    "migrator",
    "gateway",
    "runtime",
    "capability_worker",
    "knowledge_api",
    "knowledge_worker",
];
static PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_]{0,20}_$").unwrap());
pub fn validate_role_prefix(prefix: &str) -> Result<()> {
    if !PREFIX.is_match(prefix) {
        return Err(AuthorityError::Refused(
            "unsafe authority role prefix".into(),
        ));
    }
    Ok(())
}
pub fn role_prefix_from_env() -> Result<String> {
    let prefix =
        std::env::var("AI_GATEWAY_ROLE_PREFIX").unwrap_or_else(|_| DEFAULT_ROLE_PREFIX.into());
    validate_role_prefix(&prefix)?;
    Ok(prefix)
}
pub fn connection_for_principal(
    principal: &str,
) -> Result<crate::credential_files::PrincipalConnection> {
    crate::credential_files::resolve(
        &std::env::vars().collect(),
        principal,
        &role_prefix_from_env()?,
    )
}
pub fn dsn_for_principal(principal: &str) -> Result<String> {
    Ok(connection_for_principal(principal)?.dsn)
}
pub async fn connect_principal(principal: &str) -> Result<PgConnection> {
    PgConnection::connect_with(&connection_for_principal(principal)?.options)
        .await
        .map_err(|_| {
            AuthorityError::Refused("database principal connection failed; values withheld".into())
        })
}
pub async fn verify_role_connections(
    prefix: &str,
    expected_database: Option<&str>,
) -> Result<usize> {
    validate_role_prefix(prefix)?;
    let mut dsns: BTreeMap<&str, crate::credential_files::PrincipalConnection> = BTreeMap::new();
    let mut tls_keys = std::collections::BTreeSet::new();
    for &principal in ALL_PRINCIPALS {
        let connection = connection_for_principal(principal)?;
        if let Some(key) = &connection.tls_key_identity {
            if !tls_keys.insert(key.clone()) {
                return Err(AuthorityError::Refused(
                    "database principals require distinct TLS client keys".into(),
                ));
            }
        }
        if dsns.values().any(|v| v.dsn == connection.dsn) {
            return Err(AuthorityError::Refused(
                "database role DSNs must be distinct".into(),
            ));
        }
        dsns.insert(principal, connection);
    }
    if !tls_keys.is_empty() && tls_keys.len() != ALL_PRINCIPALS.len() {
        return Err(AuthorityError::Refused(
            "file TLS role verification cannot mix legacy plaintext connections".into(),
        ));
    }
    for (principal, connection) in &dsns {
        let mut conn = PgConnection::connect_with(&connection.options)
            .await
            .map_err(|_| {
                AuthorityError::Refused(
                    "database principal connection failed; values withheld".into(),
                )
            })?;
        let row = sqlx::query("SELECT current_user::text AS current_user, session_user::text AS session_user, current_database()::text AS database_name, current_setting('is_superuser', true) = 'on' AS is_superuser").fetch_one(&mut conn).await?;
        let current: String = row.try_get("current_user")?;
        let session: String = row.try_get("session_user")?;
        if current != session {
            return Err(AuthorityError::Refused(format!(
                "{principal} DSN starts under an implicit SET ROLE identity"
            )));
        }
        if let Some(expected) = expected_database {
            if row.try_get::<String, _>("database_name")? != expected {
                return Err(AuthorityError::Refused(format!(
                    "{principal} DSN does not target the expected database"
                )));
            }
        }
        if *principal == "admin" {
            if !row.try_get::<bool, _>("is_superuser")? {
                return Err(AuthorityError::Refused(
                    "admin DSN is not a PostgreSQL admin".into(),
                ));
            }
        } else {
            if current != format!("{prefix}{principal}") {
                return Err(AuthorityError::Refused(format!(
                    "{principal} DSN authenticates as an unexpected role"
                )));
            }
            let attrs = sqlx::query("SELECT rolsuper, rolcreatedb, rolcreaterole, rolreplication, rolbypassrls, rolinherit FROM pg_roles WHERE rolname=current_user").fetch_one(&mut conn).await?;
            for attr in [
                "rolsuper",
                "rolcreatedb",
                "rolcreaterole",
                "rolreplication",
                "rolbypassrls",
                "rolinherit",
            ] {
                if attrs.try_get::<bool, _>(attr)? {
                    return Err(AuthorityError::Refused(format!(
                        "{principal} role has a forbidden cluster or inheritance attribute"
                    )));
                }
            }
        }
        conn.close().await?;
    }
    Ok(dsns.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn role_prefix_is_bounded_and_sql_safe() {
        for prefix in ["ai_gateway_", "a_", "deployment_123_"] {
            assert!(validate_role_prefix(prefix).is_ok());
        }
        for prefix in [
            "",
            "owner",
            "A_",
            "_",
            "a';DROP ROLE owner;--_",
            "abcdefghijklmnopqrstuvwxyz_",
        ] {
            assert!(validate_role_prefix(prefix).is_err());
        }
    }
}
