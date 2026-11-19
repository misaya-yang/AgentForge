use crate::{
    AuthorityError, DEFAULT_BASELINE_ID, Result,
    credentials::validate_role_prefix,
    ledger,
    manifest::BaselineManifest,
    runner::{AuthorityPaths, set_role},
};
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, Row};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
#[path = "bootstrap_sql.rs"]
mod queries;
pub const SCHEMAS: &[&str] = &["gateway", "assistant", "knowledge", "public"];
pub const PRINCIPALS: &[&str] = &[
    "owner",
    "migrator",
    "gateway",
    "runtime",
    "capability_worker",
    "knowledge_api",
    "knowledge_worker",
];
const REQUIRED_OBJECTS: &[&[&str]] = &[
    &["public.services", "gateway.services"],
    &["public.datasets", "knowledge.datasets"],
    &["public.documents", "knowledge.documents"],
    &["public.segments", "knowledge.segments"],
    &["public.users"],
    &["public.permissions"],
    &["public.rbac_roles"],
];
pub fn render_role_sql(sql: &str, prefix: &str) -> Result<String> {
    validate_role_prefix(prefix)?;
    Ok(sql.replace("ai_gateway_", prefix))
}
pub async fn role_bootstrap_issues(conn: &mut PgConnection, prefix: &str) -> Result<Vec<String>> {
    validate_role_prefix(prefix)?;
    let names = PRINCIPALS
        .iter()
        .map(|p| format!("{prefix}{p}"))
        .collect::<Vec<_>>();
    let rows = sqlx::query(queries::_ROLE_BOOTSTRAP_STATE_SQL)
        .bind(&names)
        .fetch_all(&mut *conn)
        .await?;
    let mut by_name = BTreeMap::new();
    for row in rows {
        by_name.insert(row.try_get::<String, _>("rolname")?, row);
    }
    let mut issues = Vec::new();
    for name in &names {
        if !by_name.contains_key(name) {
            issues.push(format!("missing role {name}"));
        }
    }
    if !issues.is_empty() {
        return Ok(issues);
    }
    let owner = format!("{prefix}owner");
    for principal in PRINCIPALS {
        let name = format!("{prefix}{principal}");
        let row = &by_name[&name];
        if row.try_get::<bool, _>("rolcanlogin")? != (*principal != "owner") {
            issues.push(format!("{name} LOGIN/NOLOGIN attribute is incorrect"));
        }
        for field in [
            "rolinherit",
            "rolsuper",
            "rolcreatedb",
            "rolcreaterole",
            "rolreplication",
            "rolbypassrls",
        ] {
            if row.try_get::<bool, _>(field)? {
                issues.push(format!("{name} must force {field}=false"));
            }
        }
        let memberships = row.try_get::<Vec<String>, _>("memberships")?;
        let expected = if *principal == "migrator" {
            vec![owner.clone()]
        } else {
            vec![]
        };
        if memberships != expected {
            issues.push(format!("{name} membership mismatch"));
        }
        let configs = row
            .try_get::<Option<Vec<String>>, _>("rolconfig")?
            .unwrap_or_default();
        let actual = configs
            .iter()
            .find_map(|v| v.strip_prefix("search_path="))
            .map(|v| v.split(',').map(str::trim).collect::<Vec<_>>());
        let expected = if matches!(*principal, "knowledge_api" | "knowledge_worker") {
            vec!["pg_catalog", "knowledge", "gateway", "assistant", "public"]
        } else {
            vec!["pg_catalog", "gateway", "assistant", "knowledge", "public"]
        };
        if actual.as_ref() != Some(&expected) {
            issues.push(format!("{name} role-level search_path mismatch"));
        }
    }
    let schema_rows = sqlx::query(queries::_SCHEMA_BOOTSTRAP_STATE_SQL)
        .bind(SCHEMAS)
        .bind(&names)
        .fetch_all(&mut *conn)
        .await?;
    let mut present = BTreeSet::new();
    for row in schema_rows {
        let schema: String = row.try_get("nspname")?;
        present.insert(schema.clone());
        if row.try_get::<String, _>("owner")? != owner
            || row.try_get::<bool, _>("public_create")?
            || row.try_get::<Vec<String>, _>("create_roles")? != vec![owner.clone()]
        {
            issues.push(format!(
                "schema {schema} ownership/CREATE privilege mismatch"
            ));
        }
    }
    for schema in SCHEMAS {
        if !present.contains(*schema) {
            issues.push(format!("missing platform schema {schema}"));
        }
    }
    let database = sqlx::query(queries::_DATABASE_BOOTSTRAP_STATE_SQL)
        .bind(&names)
        .fetch_one(&mut *conn)
        .await?;
    if database.try_get::<bool, _>("public_create")?
        || database.try_get::<Vec<String>, _>("create_roles")? != vec![owner.clone()]
    {
        issues.push("current database CREATE privileges mismatch".into());
    }
    let defaults = sqlx::query(queries::_DEFAULT_ACL_BOOTSTRAP_STATE_SQL)
        .bind(&owner)
        .bind(SCHEMAS)
        .fetch_all(&mut *conn)
        .await?;
    if defaults.len() != SCHEMAS.len() * 4 {
        issues.push("owner default ACL matrix is incomplete".into());
    }
    for row in defaults {
        if row.try_get::<bool, _>("public_has_privilege")? {
            issues.push("PUBLIC retains an owner default privilege".into());
        }
    }
    issues.sort();
    Ok(issues)
}
pub async fn bootstrap_roles(conn: &mut PgConnection, prefix: &str) -> Result<()> {
    let issues = role_bootstrap_issues(conn, prefix).await?;
    if issues.is_empty() {
        Ok(())
    } else {
        Err(AuthorityError::Refused(format!(
            "role bootstrap is ADMIN-ONLY and incomplete: {}",
            issues.join("; ")
        )))
    }
}
pub async fn provision_roles_admin(
    conn: &mut PgConnection,
    paths: &AuthorityPaths,
    prefix: &str,
) -> Result<()> {
    if role_bootstrap_issues(conn, prefix).await?.is_empty() {
        return Ok(());
    }
    let admin: bool = sqlx::query_scalar("SELECT current_setting('is_superuser',true)='on'")
        .fetch_one(&mut *conn)
        .await?;
    if !admin {
        return Err(AuthorityError::Refused(
            "role provisioning requires a separate PostgreSQL admin connection".into(),
        ));
    }
    let sql = render_role_sql(
        &std::fs::read_to_string(paths.bootstrap_dir().join("roles.sql"))?,
        prefix,
    )?;
    sqlx::raw_sql(&sql).execute(&mut *conn).await?;
    bootstrap_roles(conn, prefix).await
}
pub async fn database_empty(conn: &mut PgConnection, allowed: &[&str]) -> Result<bool> {
    let sql = format!("SELECT ({})::bigint", queries::DATABASE_EMPTY_SQL.trim());
    let count: i64 = sqlx::query_scalar(&sql)
        .bind(allowed)
        .bind(&["plpgsql", "uuid-ossp", "pgcrypto", "pg_trgm", "vector"][..])
        .fetch_one(conn)
        .await?;
    Ok(count == 0)
}
pub async fn bootstrap_extensions(
    conn: &mut PgConnection,
    paths: &AuthorityPaths,
    prefix: &str,
) -> Result<()> {
    let mut tx = conn.begin().await?;
    set_role(&mut tx, &format!("{prefix}owner")).await?;
    sqlx::raw_sql(&std::fs::read_to_string(
        paths.bootstrap_dir().join("extensions.sql"),
    )?)
    .execute(&mut *tx)
    .await?;
    sqlx::query("RESET ROLE").execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
pub async fn run_baseline_sql_file(
    conn: &mut PgConnection,
    path: &Path,
    prefix: Option<&str>,
    execution_role: Option<&str>,
) -> Result<()> {
    let text = std::fs::read_to_string(path)?;
    let text = if let Some(prefix) = prefix {
        render_role_sql(&text, prefix)?
    } else {
        text
    };
    let mut tx = conn.begin().await?;
    if let Some(role) = execution_role {
        set_role(&mut tx, role).await?;
    }
    sqlx::raw_sql(&text).execute(&mut *tx).await?;
    if execution_role.is_some() {
        sqlx::query("RESET ROLE").execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}
pub async fn verify_baseline_sql_file(
    conn: &mut PgConnection,
    path: &Path,
    prefix: &str,
) -> Result<()> {
    verify_baseline_sql_file_with_acl(conn, path, prefix, None).await
}
pub async fn verify_baseline_sql_file_with_acl(
    conn: &mut PgConnection,
    path: &Path,
    prefix: &str,
    expected: Option<&BTreeMap<String, String>>,
) -> Result<()> {
    if let Some(expected) = expected {
        crate::ledger_acl::compatible_expected(conn, expected, prefix).await?;
    }
    let sql = render_role_sql(&std::fs::read_to_string(path)?, prefix)?;
    let uncommented = sql
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    let select = uncommented.trim().trim_end_matches(';').trim();
    if !select.to_uppercase().starts_with("SELECT ") || select.contains(';') {
        return Err(AuthorityError::Refused(
            "baseline verification must contain exactly one read-only SELECT".into(),
        ));
    }
    let mut tx = conn.begin().await?;
    let previous: String = sqlx::query_scalar("SELECT current_setting('role')")
        .fetch_one(&mut *tx)
        .await?;
    set_role(&mut tx, &format!("{prefix}owner")).await?;
    let rows = sqlx::query(&sql).fetch_all(&mut *tx).await?;
    if rows.is_empty() {
        return Err(AuthorityError::Refused(
            "baseline verify returned zero checks".into(),
        ));
    }
    let mut names = BTreeSet::new();
    for row in rows {
        let name: String = row.try_get("check_name")?;
        let verdict = row.try_get::<bool, _>("ok").ok();
        let proven_delta = name == "grant-matrix" && expected.is_some() && verdict.is_some();
        if name.is_empty()
            || !names.insert(name.clone())
            || (verdict != Some(true) && !proven_delta)
        {
            return Err(AuthorityError::Refused(format!(
                "baseline verify failed or returned malformed check: {name}"
            )));
        }
    }
    set_role(&mut tx, &previous).await?;
    tx.commit().await?;
    Ok(())
}
pub async fn startup_schema_check(conn: &mut PgConnection, min: i32, max: i32) -> Result<Value> {
    let mut missing = Vec::new();
    for candidates in REQUIRED_OBJECTS {
        let mut found = false;
        for qualified in *candidates {
            if sqlx::query_scalar::<_, bool>("SELECT to_regclass($1) IS NOT NULL")
                .bind(qualified)
                .fetch_one(&mut *conn)
                .await?
            {
                found = true;
                break;
            }
        }
        if !found {
            missing.push(candidates.join(" or "));
        }
    }
    let mut result = json!({"ok":true,"epoch":null,"missing_objects":missing});
    let present = sqlx::query_scalar::<_, bool>(
        "SELECT to_regclass('public.platform_schema_baselines') IS NOT NULL",
    )
    .fetch_one(&mut *conn)
    .await?;
    if !present {
        if !missing.is_empty() || min > 0 {
            result["ok"] = json!(false);
            result["reason"] =
                json!("schema authority migration required before application startup");
        } else {
            result["epoch"] = json!("legacy");
        }
        return Ok(result);
    }
    let baselines: Vec<String> =
        sqlx::query_scalar("SELECT baseline_id FROM public.platform_schema_baselines")
            .fetch_all(&mut *conn)
            .await?;
    let Some(baseline) = baselines.first() else {
        result["ok"] = json!(false);
        result["reason"] = json!(
            "migration authority ledger exists but no baseline is adopted; run the migrator before starting the application"
        );
        return Ok(result);
    };
    if baseline != DEFAULT_BASELINE_ID {
        result["ok"] = json!(false);
        result["reason"] = json!("adopted baseline is not in the supported set; refusing to start");
        return Ok(result);
    }
    let sequence: i32 = sqlx::query_scalar(
        "SELECT COALESCE(max(sequence),0) FROM public.platform_schema_changes WHERE baseline_id=$1",
    )
    .bind(baseline)
    .fetch_one(&mut *conn)
    .await?;
    if !(min..=max).contains(&sequence) {
        result["ok"] = json!(false);
        result["reason"] = json!(format!(
            "database schema revision {baseline}:{sequence} is outside the application window ({min}..{max}); refusing to start"
        ));
        return Ok(result);
    }
    result["epoch"] = json!(format!("{baseline}:{sequence}"));
    if !missing.is_empty() {
        result["ok"] = json!(false)
    }
    Ok(result)
}

pub fn validate_adoption_marker(
    marker: &Value,
    baseline: &BaselineManifest,
    sha: &str,
) -> Result<()> {
    for (field, expected) in [
        ("baseline_id", baseline.baseline_id.as_str()),
        ("manifest_sha256", sha),
        ("structural_sha256", &baseline.structural_sha256),
        ("acl_sha256", &baseline.acl_sha256),
        ("extensions_sha256", &baseline.extensions_sha256),
        ("reference_data_sha256", &baseline.reference_data_sha256),
        ("source_git_sha", &baseline.source_git_sha),
    ] {
        if marker.get(field).and_then(Value::as_str) != Some(expected) {
            return Err(AuthorityError::Refused(format!(
                "adopted baseline marker does not match frozen manifest ({field})"
            )));
        }
    }
    Ok(())
}
pub async fn fresh_install(
    conn: &mut PgConnection,
    paths: &AuthorityPaths,
    baseline: &BaselineManifest,
    manifest_sha: &str,
    prefix: &str,
) -> Result<BTreeMap<String, String>> {
    bootstrap_roles(conn, prefix).await?;
    let previous: String = sqlx::query_scalar("SELECT current_setting('role')")
        .fetch_one(&mut *conn)
        .await?;
    let mut tx = conn.begin().await?;
    let owner = format!("{prefix}owner");
    set_role(&mut tx, &owner).await?;
    let ledger_present = crate::runner::table_exists(&mut tx, ledger::BASELINES_TABLE).await?;
    if ledger_present {
        for table in [
            ledger::BASELINES_TABLE,
            ledger::CHANGES_TABLE,
            ledger::ATTEMPTS_TABLE,
        ] {
            if !crate::runner::table_exists(&mut tx, table).await? {
                return Err(AuthorityError::Refused(
                    "fresh-install marker ledger is incomplete".into(),
                ));
            }
        }
        let markers: Vec<Value> =
            sqlx::query_scalar("SELECT to_jsonb(b) FROM public.platform_schema_baselines b")
                .fetch_all(&mut *tx)
                .await?;
        if markers.len() != 1 {
            return Err(AuthorityError::Refused(
                "fresh-install ledger must contain exactly one adoption marker".into(),
            ));
        }
        validate_adoption_marker(&markers[0], baseline, manifest_sha)?;
        let rows = sqlx::query(ledger::SELECT_APPLIED_CHANGES)
            .bind(&baseline.baseline_id)
            .fetch_all(&mut *tx)
            .await?;
        let applied = rows
            .into_iter()
            .map(|r| Ok((r.try_get("sequence")?, r.try_get("checksum_sha256")?)))
            .collect::<Result<BTreeMap<i32, String>>>()?;
        let expected = crate::commands::expected_epoch_fingerprints(paths, baseline, &applied)?;
        let expected = crate::ledger_acl::compatible_expected(&mut tx, &expected, prefix).await?;
        let computed =
            crate::fingerprint::compute_fingerprints(&mut tx, prefix, &baseline.reference_data)
                .await?;
        if expected != computed {
            return Err(AuthorityError::Refused(
                "fresh-install marker exists but live fingerprints drifted".into(),
            ));
        }
        set_role(&mut tx, &previous).await?;
        tx.commit().await?;
        return Ok(computed);
    }
    if !database_empty(&mut tx, SCHEMAS).await? {
        return Err(AuthorityError::Refused(
            "database is not empty; init.sql must never run against a non-empty database".into(),
        ));
    }
    bootstrap_extensions(&mut tx, paths, prefix).await?;
    set_role(&mut tx, &owner).await?;
    let directory = paths.baseline_dir(&baseline.baseline_id);
    for file in ["init.sql", "reference_data.sql"] {
        run_baseline_sql_file(&mut tx, &directory.join(file), None, None).await?;
    }
    run_baseline_sql_file(
        &mut tx,
        &directory.join("grants.sql"),
        Some(prefix),
        Some(&owner),
    )
    .await?;
    set_role(&mut tx, &owner).await?;
    verify_baseline_sql_file(&mut tx, &directory.join("verify.sql"), prefix).await?;
    let computed =
        crate::fingerprint::compute_fingerprints(&mut tx, prefix, &baseline.reference_data).await?;
    if computed != baseline.fingerprints() {
        let expected = baseline.fingerprints();
        let categories = expected
            .iter()
            .filter_map(|(kind, digest)| {
                (computed.get(kind) != Some(digest)).then(|| {
                    format!(
                        "{kind}: expected={digest}, observed={}",
                        computed.get(kind).map(String::as_str).unwrap_or("missing")
                    )
                })
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(AuthorityError::Refused(format!(
            "fresh install fingerprint mismatch: {categories}"
        )));
    }
    sqlx::raw_sql(ledger::LEDGER_DDL).execute(&mut *tx).await?;
    crate::ledger_acl::install(&mut tx, prefix).await?;
    set_role(&mut tx, &previous).await?;
    insert_baseline_marker(&mut tx, baseline, manifest_sha).await?;
    tx.commit().await?;
    Ok(computed)
}
pub async fn insert_baseline_marker(
    conn: &mut PgConnection,
    baseline: &BaselineManifest,
    sha: &str,
) -> Result<()> {
    sqlx::query(ledger::INSERT_BASELINE_MARKER)
        .bind(&baseline.baseline_id)
        .bind(sha)
        .bind(&baseline.structural_sha256)
        .bind(&baseline.acl_sha256)
        .bind(&baseline.extensions_sha256)
        .bind(&baseline.reference_data_sha256)
        .bind(&baseline.source_git_sha)
        .execute(&mut *conn)
        .await?;
    let rows: Vec<Value> =
        sqlx::query_scalar("SELECT to_jsonb(b) FROM public.platform_schema_baselines b")
            .fetch_all(conn)
            .await?;
    if rows.len() != 1 {
        return Err(AuthorityError::Refused(
            "exactly one baseline can be authoritative".into(),
        ));
    }
    validate_adoption_marker(&rows[0], baseline, sha)
}

pub async fn provision_extensions_admin(
    conn: &mut PgConnection,
    paths: &AuthorityPaths,
) -> Result<()> {
    let admin: bool = sqlx::query_scalar("SELECT current_setting('is_superuser',true)='on'")
        .fetch_one(&mut *conn)
        .await?;
    if !admin {
        return Err(AuthorityError::Refused(
            "extension provisioning requires separate PostgreSQL admin connection".into(),
        ));
    }
    let mut tx = conn.begin().await?;
    sqlx::raw_sql(&std::fs::read_to_string(
        paths.bootstrap_dir().join("extensions.sql"),
    )?)
    .execute(&mut *tx)
    .await?;
    sqlx::raw_sql(queries::_ROUTINE_ACL_HARDENING_SQL)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
