//! Explicit copy of an existing disposable successor reference, never an app DB.
use ai_platform_migration_authority::{credentials, source_tools::measure_successors};
use sqlx::{Connection, PgConnection};
use std::collections::BTreeMap;

fn options(
    arguments: impl IntoIterator<Item = String>,
) -> Result<BTreeMap<String, String>, &'static str> {
    let mut args = arguments.into_iter();
    let mut values = BTreeMap::new();
    while let Some(name) = args.next() {
        if !["--template", "--database", "--expected-admin"].contains(&name.as_str())
            || values.contains_key(&name)
        {
            return Err("invalid_or_duplicate_option");
        }
        let value = args
            .next()
            .filter(|v| !v.is_empty() && !v.starts_with('-'))
            .ok_or("missing_value")?;
        values.insert(name, value);
    }
    if values.len() != 3 {
        return Err("required_template_database_and_expected_admin");
    }
    for key in ["--template", "--database"] {
        measure_successors::validate_database_name(&values[key])
            .map_err(|_| "isolated_database_name_required")?;
    }
    if values["--template"] == values["--database"] {
        return Err("source_and_target_must_differ");
    }
    Ok(values)
}

async fn execute(values: BTreeMap<String, String>) -> Result<(), &'static str> {
    let dsn = credentials::dsn_for_principal("admin").map_err(|_| "explicit_admin_dsn_required")?;
    let url = url::Url::parse(&dsn).map_err(|_| "invalid_admin_dsn")?;
    if !["postgres", "postgresql"].contains(&url.scheme())
        || url.host_str() != Some("127.0.0.1")
        || url.username() != values["--expected-admin"]
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("explicit_loopback_administrator_required");
    }
    let mut connection = credentials::connect_principal("admin")
        .await
        .map_err(|_| "admin_connection_failed")?;
    let (user,allowed):(String,bool)=sqlx::query_as("SELECT current_user::text,rolsuper OR rolcreatedb FROM pg_catalog.pg_roles WHERE rolname=current_user")
        .fetch_one(&mut connection).await.map_err(|_|"admin_identity_failed")?;
    if user != values["--expected-admin"] || !allowed {
        return Err("existing_database_creation_privilege_required");
    }
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_database WHERE datname=$1)")
            .bind(&values["--database"])
            .fetch_one(&mut connection)
            .await
            .map_err(|_| "target_probe_failed")?;
    if exists {
        return Err("target_already_exists_no_replacement");
    }
    let template:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_database WHERE datname=$1 AND datallowconn AND NOT datistemplate)")
        .bind(&values["--template"]).fetch_one(&mut connection).await.map_err(|_|"template_probe_failed")?;
    if !template {
        return Err("disposable_reference_missing");
    }
    // Both identifiers passed the authority's strict ASCII-name validator. Do
    // not terminate sessions, copy production data, change roles or grant rights.
    sqlx::raw_sql(&format!(
        "CREATE DATABASE \"{}\" TEMPLATE \"{}\"",
        values["--database"], values["--template"]
    ))
    .execute(&mut connection)
    .await
    .map_err(|_| "isolated_reference_copy_failed")?;
    connection.close().await.map_err(|_| "admin_close_failed")?;
    println!("isolated successor reference copied; main application database unchanged");
    Ok(())
}
#[tokio::main]
async fn main() {
    let result = match options(std::env::args().skip(1)) {
        Ok(options) => execute(options).await,
        Err(e) => Err(e),
    };
    if let Err(code) = result {
        eprintln!("successor test database: {code}; credentials and driver details withheld");
        std::process::exit(2);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(template: &str, target: &str) -> Vec<String> {
        [
            "--template",
            template,
            "--database",
            target,
            "--expected-admin",
            "test_admin",
        ]
        .map(str::to_owned)
        .to_vec()
    }
    #[test]
    fn only_distinct_disposable_names_can_be_copied() {
        assert!(
            options(args(
                "schema_successor_verify_source028",
                "schema_successor_verify_target029"
            ))
            .is_ok()
        );
        for (a, b) in [
            ("gateway", "schema_successor_verify_target029"),
            ("schema_successor_verify_source028", "gateway"),
            (
                "schema_successor_verify_source028",
                "schema_successor_verify_source028",
            ),
            (
                "schema_successor_verify_source028",
                "schema_successor_verify_bad\";DROP DATABASE gateway;",
            ),
        ] {
            assert!(options(args(a, b)).is_err());
        }
    }
    #[test]
    fn unknown_and_duplicate_options_never_select_a_mutation() {
        let mut a = args(
            "schema_successor_verify_source028",
            "schema_successor_verify_target029",
        );
        a.extend(["--force".into(), "true".into()]);
        assert!(options(a).is_err());
    }
}
