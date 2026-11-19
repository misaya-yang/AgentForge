//! Database creation is a separate local OS/operator privilege within the single
//! native entry point. It never grants CREATEDB to the migrator or uses TCP admin.
use super::*;
use sqlx::{
    Connection, Row,
    postgres::{PgConnectOptions, PgSslMode},
};
use std::path::{Component, PathBuf};
pub struct LocalOperator {
    pub socket_directory: PathBuf,
    pub username: String,
}
impl LocalOperator {
    pub fn from_env() -> Result<Self> {
        let value = Self {
            socket_directory: std::env::var_os("AI_GATEWAY_FRESH_OPERATOR_SOCKET_DIRECTORY")
                .map(Into::into)
                .unwrap_or_else(|| PathBuf::from("/var/run/postgresql")),
            username: std::env::var("AI_GATEWAY_FRESH_OPERATOR_USER")
                .unwrap_or_else(|_| "postgres".into()),
        };
        value.validate()?;
        Ok(value)
    }
    fn validate(&self) -> Result<()> {
        if !self.socket_directory.is_absolute()
            || self
                .socket_directory
                .components()
                .any(|p| matches!(p, Component::ParentDir))
        {
            return Err(refused(
                "operator endpoint must be an absolute local Unix socket directory",
            ));
        }
        crate::manifest::validate_identifier(&self.username)?;
        Ok(())
    }
}
pub async fn initialize_with_local_operator(
    expected_database: &str,
    operator: &LocalOperator,
) -> Result<Value> {
    install::target(expected_database)?;
    operator.validate()?;
    execution::stage(
        "fresh.operator-and-initialize",
        execution::OPERATOR_LIMIT,
        initialize_with_local_operator_inner(expected_database, operator),
    )
    .await
}
async fn initialize_with_local_operator_inner(
    expected_database: &str,
    operator: &LocalOperator,
) -> Result<Value> {
    // Validate the complete compiled source before any operator/database action.
    source::compiled_definitions()?;
    let prefix = crate::credentials::role_prefix_from_env()?;
    // Resolve all private files first. This exposes no credentials to argv/env
    // and rejects a bad file/mTLS contract before creating a new database.
    for principal in std::iter::once("migrator")
        .chain(crate::credentials::APPLICATION_PRINCIPALS.iter().copied())
    {
        let c = crate::credentials::connection_for_principal(principal)?;
        if c.options.get_database() != Some(DATABASE) {
            return Err(refused("principal file target must name gateway_dev_fresh"));
        }
    }
    let options = PgConnectOptions::new()
        .host(
            operator
                .socket_directory
                .to_str()
                .ok_or_else(|| refused("operator socket path encoding invalid"))?,
        )
        .port(5432)
        .username(&operator.username)
        .password("")
        .database("postgres")
        .ssl_mode(PgSslMode::Disable)
        .application_name("agentforge_fresh_local_operator");
    let mut conn=sqlx::PgConnection::connect_with(&options).await.map_err(|_|refused("local Unix socket operator connection failed; use the existing PostgreSQL operator OS identity/socket mount, not TCP superuser"))?;
    let row=sqlx::query("SELECT current_database()::text AS database,current_user::text AS identity,session_user::text AS session,inet_client_addr() IS NULL AND inet_server_addr() IS NULL AS local_socket,r.rolsuper FROM pg_catalog.pg_roles r WHERE r.rolname=current_user")
        .fetch_one(&mut conn).await?;
    if row.try_get::<String, _>("database")? != "postgres"
        || row.try_get::<String, _>("identity")? != operator.username
        || row.try_get::<String, _>("session")? != operator.username
        || !row.try_get::<bool, _>("local_socket")?
        || !row.try_get::<bool, _>("rolsuper")?
    {
        return Err(refused(
            "database creation requires the independent local PostgreSQL operator",
        ));
    }
    let owner = format!("{prefix}owner");
    let safe_owner:bool=sqlx::query_scalar("SELECT NOT(rolcanlogin OR rolinherit OR rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls) FROM pg_catalog.pg_roles WHERE rolname=$1")
        .bind(&owner).fetch_optional(&mut conn).await?.unwrap_or(false);
    if !safe_owner {
        return Err(refused(
            "existing canonical NOLOGIN owner is missing or privileged; no role mutation performed",
        ));
    }
    sqlx::query("SELECT pg_catalog.pg_advisory_lock($1,$2)")
        .bind(crate::LOCK_NAMESPACE)
        .bind(crate::LOCK_ID)
        .execute(&mut conn)
        .await?;
    let existing:Option<String>=sqlx::query_scalar("SELECT pg_catalog.pg_get_userbyid(datdba)::text FROM pg_catalog.pg_database WHERE datname=$1")
        .bind(DATABASE).fetch_optional(&mut conn).await?;
    let created = if let Some(actual) = existing {
        if actual != owner {
            return Err(refused(
                "existing dedicated database belongs to another owner",
            ));
        }
        false
    } else {
        sqlx::raw_sql(&format!(
            "CREATE DATABASE {DATABASE} OWNER \"{owner}\" TEMPLATE template0 ENCODING 'UTF8'"
        ))
        .execute(&mut conn)
        .await?;
        true
    };
    // Connection close releases the operator lock. No password, role attribute,
    // old database, clone, HBA rule or existing volume is modified.
    execution::stage("operator.close", std::time::Duration::from_secs(3), async {
        Ok(conn.close().await?)
    })
    .await?;
    let mut value = install::initialize_with_operator_connection(
        expected_database,
        &options.clone().database(DATABASE),
    )
    .await?;
    value["database_created_by_local_operator"] = json!(created);
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operator_endpoint_cannot_be_a_tcp_admin_or_path_escape() {
        for p in [
            "localhost",
            "127.0.0.1",
            "postgresql://postgres@localhost",
            "/run/../postgres",
        ] {
            assert!(
                LocalOperator {
                    socket_directory: p.into(),
                    username: "postgres".into()
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            LocalOperator {
                socket_directory: "/var/run/postgresql".into(),
                username: "postgres".into()
            }
            .validate()
            .is_ok()
        );
    }
}
