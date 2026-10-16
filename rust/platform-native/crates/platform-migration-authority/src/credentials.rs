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
pub fn dsn_for_principal(principal: &str) -> Result<String> {
    if !ALL_PRINCIPALS.contains(&principal) {
        return Err(AuthorityError::Refused("unknown database principal".into()));
    }
    let key = format!("AI_GATEWAY_DATABASE_{}_DSN", principal.to_ascii_uppercase());
    std::env::var(&key)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            AuthorityError::Refused(format!(
                "{key} is required; shared or inferred database credentials are forbidden"
            ))
        })
}
pub async fn verify_role_connections(
    prefix: &str,
    expected_database: Option<&str>,
) -> Result<usize> {
    validate_role_prefix(prefix)?;
    let mut dsns = BTreeMap::new();
    for &principal in ALL_PRINCIPALS {
        let dsn = dsn_for_principal(principal)?;
        if dsns.values().any(|v| v == &dsn) {
            return Err(AuthorityError::Refused(
                "database role DSNs must be distinct".into(),
            ));
        }
        dsns.insert(principal, dsn);
    }
    for (principal, dsn) in &dsns {
        let mut conn = PgConnection::connect(dsn).await?;
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
