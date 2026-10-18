//! Opt-in local login initialization. Never rotates an existing credential or changes role privileges.
use crate::{AuthorityError, Result, bootstrap, credentials, runner::MigrationAuthority};
use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use sqlx::{Connection, Row};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use subtle::ConstantTimeEq;

const ITERATIONS: u32 = 4096;
struct Login {
    role: String,
    password: String,
}
struct Plan {
    admin_user: String,
    admin_port: u16,
    logins: Vec<Login>,
}
fn refuse(message: &str) -> AuthorityError {
    AuthorityError::Refused(message.into())
}
fn parse_dsn(raw: &str) -> Result<url::Url> {
    let url = url::Url::parse(raw)
        .map_err(|_| refuse("invalid local credential DSN; contents withheld"))?;
    if !["postgres", "postgresql"].contains(&url.scheme())
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str() != Some("127.0.0.1")
    {
        return Err(refuse(
            "local credentials require the numeric published 127.0.0.1 PostgreSQL address without query overrides",
        ));
    }
    Ok(url)
}
fn plan(values: &BTreeMap<String, String>, prefix: &str, expected: &str) -> Result<Plan> {
    credentials::validate_role_prefix(prefix)?;
    let dsn = |principal: &str| -> Result<&str> {
        values.get(&format!("AI_GATEWAY_DATABASE_{}_DSN",principal.to_ascii_uppercase()))
            .map(String::as_str).filter(|s| !s.is_empty()).ok_or_else(|| refuse("all distinct authority and application DSNs are required for local credentials"))
    };
    let admin = parse_dsn(dsn("admin")?)?;
    if expected.is_empty()
        || admin.path().strip_prefix('/') != Some(expected)
        || admin.username().is_empty()
    {
        return Err(refuse(
            "administrator DSN does not match the explicitly expected local database",
        ));
    }
    let password = |url: &url::Url| -> Result<String> {
        let raw = url
            .password()
            .ok_or_else(|| refuse("every local role needs an explicit private password"))?;
        let value = percent_encoding::percent_decode_str(raw)
            .decode_utf8()
            .map_err(|_| refuse("invalid private password encoding"))?
            .into_owned();
        if value.len() < 32
            || value.len() > 1024
            || !value.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            return Err(refuse(
                "local role passwords must be 32 to 1024 printable ASCII characters",
            ));
        }
        Ok(value)
    };
    let mut passwords = BTreeSet::from([password(&admin)?]);
    let mut logins = vec![];
    for principal in credentials::ALL_PRINCIPALS
        .iter()
        .copied()
        .filter(|p| *p != "admin")
    {
        let url = parse_dsn(dsn(principal)?)?;
        let role = format!("{prefix}{principal}");
        if url.host_str() != admin.host_str()
            || url.port_or_known_default().unwrap_or(5432)
                != admin.port_or_known_default().unwrap_or(5432)
            || url.path() != admin.path()
            || url.username() != role
        {
            return Err(refuse(
                "local role DSNs must target the same explicit database with their exact scoped identities",
            ));
        }
        let password = password(&url)?;
        if !passwords.insert(password.clone()) {
            return Err(refuse("shared local role passwords are forbidden"));
        }
        logins.push(Login { role, password });
    }
    Ok(Plan {
        admin_user: admin.username().into(),
        admin_port: admin.port_or_known_default().unwrap_or(5432),
        logins,
    })
}

fn validate_container(root: &std::path::Path, port: u16, value: &serde_json::Value) -> Result<()> {
    let config = root.join("deploy/local/compose.yml");
    let runtime = root.join("deploy/local/compose.runtime.yml");
    let allowed_configs = value["config_files"].as_str().is_some_and(|s| {
        let files: Vec<_> = s.split(',').collect();
        files == [config.to_str().unwrap_or("")]
            || files
                == [
                    config.to_str().unwrap_or(""),
                    runtime.to_str().unwrap_or(""),
                ]
    });
    let port_text = port.to_string();
    let ports = value["ports"]["5432/tcp"]
        .as_array()
        .filter(|p| !p.is_empty());
    if value["project"] != "agentforge-dev-7801d4a9a4"
        || value["running"] != true
        || value["working_dir"].as_str() != root.to_str()
        || !allowed_configs
        || !ports.is_some_and(|p| {
            p.iter()
                .all(|binding| binding["HostIp"] == "127.0.0.1" && binding["HostPort"] == port_text)
        })
    {
        return Err(refuse(
            "local PostgreSQL must be the running loopback port of this repository's exact Compose project/config",
        ));
    }
    Ok(())
}

pub async fn verify_local_target(
    paths: &crate::runner::AuthorityPaths,
    prefix: &str,
    expected: &str,
) -> Result<()> {
    let plan = plan(
        &crate::credential_files::bootstrap_values(&std::env::vars().collect(), prefix)?,
        prefix,
        expected,
    )?;
    // Local opt-in is source-bound. An environment artifact-path override must not
    // redefine which checkout's containers are trusted for database mutations.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()?;
    if paths.database_dir.canonicalize()? != root.join("database").canonicalize()? {
        return Err(refuse(
            "local credentials require this binary's repository database source",
        ));
    }
    let docker = |args: Vec<String>| async move {
        tokio::time::timeout(
            Duration::from_secs(15),
            tokio::process::Command::new("docker")
                .args(args)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| refuse("Docker ownership query timed out"))?
        .map_err(|_| refuse("Docker ownership query unavailable"))
    };
    let output = docker(vec![
        "ps".into(),
        "--quiet".into(),
        "--filter".into(),
        "label=com.docker.compose.project=agentforge-dev-7801d4a9a4".into(),
        "--filter".into(),
        "label=com.docker.compose.service=postgres".into(),
    ])
    .await?;
    if !output.status.success() {
        return Err(refuse("Docker ownership query failed"));
    }
    let ids = String::from_utf8(output.stdout)
        .map_err(|_| refuse("invalid Docker ownership response"))?;
    let ids: Vec<_> = ids.split_whitespace().collect();
    if ids.len() != 1 || !ids[0].bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(refuse(
            "exactly one running PostgreSQL container in the selected project is required",
        ));
    }
    let format = r#"{"project":{{json (index .Config.Labels "com.docker.compose.project")}},"working_dir":{{json (index .Config.Labels "com.docker.compose.project.working_dir")}},"config_files":{{json (index .Config.Labels "com.docker.compose.project.config_files")}},"running":{{json .State.Running}},"ports":{{json .NetworkSettings.Ports}}}"#;
    let output = docker(vec![
        "inspect".into(),
        "--format".into(),
        format.into(),
        ids[0].into(),
    ])
    .await?;
    if !output.status.success() {
        return Err(refuse("PostgreSQL ownership inspect failed"));
    }
    let value = serde_json::from_slice(&output.stdout)
        .map_err(|_| refuse("invalid PostgreSQL ownership metadata"))?;
    validate_container(&root, plan.admin_port, &value)
}
fn keys(password: &[u8], salt: &[u8], iterations: u32) -> ([u8; 32], [u8; 32]) {
    let mut salted = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password, salt, iterations, &mut salted);
    let mut client = Hmac::<Sha256>::new_from_slice(&salted).expect("HMAC accepts any key length");
    client.update(b"Client Key");
    let stored: [u8; 32] = Sha256::digest(client.finalize().into_bytes()).into();
    let mut server = Hmac::<Sha256>::new_from_slice(&salted).expect("HMAC accepts any key length");
    server.update(b"Server Key");
    (stored, server.finalize().into_bytes().into())
}
fn verifier(password: &str, salt: &[u8]) -> String {
    let (stored, server) = keys(password.as_bytes(), salt, ITERATIONS);
    format!(
        "SCRAM-SHA-256${ITERATIONS}:{}${}:{}",
        STANDARD.encode(salt),
        STANDARD.encode(stored),
        STANDARD.encode(server)
    )
}
fn matches(password: &str, stored: &str) -> bool {
    let Some(rest) = stored.strip_prefix("SCRAM-SHA-256$") else {
        return false;
    };
    let Some((parameters, encoded)) = rest.split_once('$') else {
        return false;
    };
    let Some((iterations, salt)) = parameters.split_once(':') else {
        return false;
    };
    let Ok(iterations) = iterations.parse::<u32>() else {
        return false;
    };
    if !(4096..=1_000_000).contains(&iterations) {
        return false;
    }
    let Ok(salt) = STANDARD.decode(salt) else {
        return false;
    };
    if !(16..=64).contains(&salt.len()) {
        return false;
    }
    let Some((expected_stored, expected_server)) = encoded.split_once(':') else {
        return false;
    };
    let (Ok(expected_stored), Ok(expected_server)) = (
        STANDARD.decode(expected_stored),
        STANDARD.decode(expected_server),
    ) else {
        return false;
    };
    let (actual_stored, actual_server) = keys(password.as_bytes(), &salt, iterations);
    bool::from(
        actual_stored.as_slice().ct_eq(&expected_stored)
            & actual_server.as_slice().ct_eq(&expected_server),
    )
}

pub async fn initialize(
    authority: &MigrationAuthority,
    expected: &str,
) -> Result<serde_json::Value> {
    verify_local_target(&authority.paths, &authority.role_prefix, expected).await?;
    // Validate all targets before opening an administrator connection.
    let plan = plan(
        &crate::credential_files::bootstrap_values(
            &std::env::vars().collect(),
            &authority.role_prefix,
        )?,
        &authority.role_prefix,
        expected,
    )?;
    let mut conn = authority.connect(false).await?;
    authority.acquire_lock(&mut conn).await?;
    let outcome=async {
        let state=sqlx::query("SELECT current_user::text AS identity, session_user::text AS session, current_database()::text AS database, current_setting('is_superuser')='on' AS admin").fetch_one(&mut conn).await?;
        if !state.try_get::<bool,_>("admin")? || state.try_get::<String,_>("identity")?!=plan.admin_user
            || state.try_get::<String,_>("session")?!=plan.admin_user || state.try_get::<String,_>("database")?!=expected {
            return Err(refuse("local credential initialization requires the explicit administrator and expected database"));
        }
        if !bootstrap::role_bootstrap_issues(&mut conn,&authority.role_prefix).await?.is_empty() {
            return Err(refuse("native role provisioning must pass before local credential initialization"));
        }
        let mut tx=conn.begin().await?;
        // Serialize the missing-password check with every role update, including an external DBA.
        sqlx::query("LOCK TABLE pg_catalog.pg_authid IN SHARE ROW EXCLUSIVE MODE").execute(&mut *tx).await?;
        let mut initialized=0;
        let mut preserved=0;
        for login in &plan.logins {
            let stored: Option<String>=sqlx::query_scalar("SELECT rolpassword FROM pg_catalog.pg_authid WHERE rolname=$1 AND rolcanlogin").bind(&login.role).fetch_one(&mut *tx).await?;
            if let Some(stored)=stored {
                if !matches(&login.password,&stored) { return Err(refuse("existing local role credential does not match its DSN; no passwords rotated")); }
                preserved+=1;
            } else {
                let salt=*uuid::Uuid::new_v4().as_bytes();
                let encoded=verifier(&login.password,&salt);
                // Send only a SCRAM verifier, never the plaintext password, in SQL.
                let statement: String=sqlx::query_scalar("SELECT pg_catalog.format('ALTER ROLE %I PASSWORD %L', $1::text, $2::text)").bind(&login.role).bind(encoded).fetch_one(&mut *tx).await?;
                sqlx::raw_sql(&statement).execute(&mut *tx).await?;
                initialized+=1;
            }
        }
        tx.commit().await?;
        let verified=tokio::time::timeout(Duration::from_secs(20),credentials::verify_role_connections(&authority.role_prefix,Some(expected))).await.map_err(|_| refuse("local role connection verification timed out"))??;
        Ok(serde_json::json!({"result":"pass","command":"initialize-local-credentials","initialized":initialized,"preserved":preserved,"verified_connections":verified,"passwords_rotated":false,"values_withheld":true}))
    }.await;
    let release = authority.release_lock(conn).await;
    let result = outcome?;
    release?;
    Ok(result)
}

// Public deployment bootstrap is separate from the Docker-owned loopback local
// initializer above. It never weakens or calls that initializer's target guard.
fn deployment_plan(
    values: &BTreeMap<String, String>,
    prefix: &str,
    expected: &str,
    expected_admin: &str,
) -> Result<Plan> {
    credentials::validate_role_prefix(prefix)?;
    let identifier = |value: &str| {
        !value.is_empty()
            && value.len() <= 63
            && value.as_bytes()[0].is_ascii_alphabetic()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    };
    if !identifier(expected)
        || !identifier(expected_admin)
        || expected_admin.starts_with(prefix)
        || values
            .get("AI_PLATFORM_LOCAL_DEV_CREDENTIALS")
            .is_some_and(|v| v == "true")
    {
        return Err(refuse(
            "deployment bootstrap requires an explicit deployment database/admin and cannot replace local credential initialization",
        ));
    }
    let parse = |principal: &str| -> Result<(url::Url, String)> {
        let raw = values
            .get(&format!(
                "AI_GATEWAY_DATABASE_{}_DSN",
                principal.to_ascii_uppercase()
            ))
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                refuse("deployment bootstrap requires all seven distinct explicit DSNs")
            })?;
        let url =
            url::Url::parse(raw).map_err(|_| refuse("invalid deployment DSN; values withheld"))?;
        if !["postgres", "postgresql"].contains(&url.scheme())
            || url.host_str().is_none()
            || url.fragment().is_some()
            || url.path().strip_prefix('/') != Some(expected)
            || url.query_pairs().any(|(key, value)| {
                key != "sslmode"
                    || !["disable", "require", "verify-ca", "verify-full"].contains(&value.as_ref())
            })
            || url.query_pairs().count() > 1
        {
            return Err(refuse(
                "deployment DSNs must name the exact expected database without endpoint/identity overrides",
            ));
        }
        let password = percent_encoding::percent_decode_str(
            url.password()
                .ok_or_else(|| refuse("deployment passwords must be explicit"))?,
        )
        .decode_utf8()
        .map_err(|_| refuse("invalid deployment password encoding"))?
        .into_owned();
        if !(32..=1024).contains(&password.len())
            || !password.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            return Err(refuse(
                "deployment passwords must be 32 to 1024 printable ASCII characters",
            ));
        }
        Ok((url, password))
    };
    let (admin, password) = parse("admin")?;
    if admin.username() != expected_admin {
        return Err(refuse(
            "administrator DSN differs from explicitly expected bootstrap administrator",
        ));
    }
    let mut passwords = BTreeSet::from([password]);
    let mut logins = Vec::new();
    for principal in credentials::ALL_PRINCIPALS
        .iter()
        .copied()
        .filter(|p| *p != "admin")
    {
        let (url, password) = parse(principal)?;
        let role = format!("{prefix}{principal}");
        if url.username() != role
            || url.host_str() != admin.host_str()
            || url.port().unwrap_or(5432) != admin.port().unwrap_or(5432)
            || url.query() != admin.query()
            || !passwords.insert(password.clone())
        {
            return Err(refuse(
                "deployment roles require distinct passwords and exact scoped identities at the same explicit endpoint",
            ));
        }
        logins.push(Login { role, password });
    }
    Ok(Plan {
        admin_user: expected_admin.into(),
        admin_port: admin.port().unwrap_or(5432),
        logins,
    })
}

/// Decide before any role/schema DDL. Existing roles are validated, never
/// repaired; unknown/partially provisioned cluster scopes need explicit repair.
fn deployment_scope(
    present: &BTreeSet<String>,
    prefix: &str,
    empty: bool,
    canonical: bool,
    adopted_verified: bool,
) -> Result<bool> {
    let expected: BTreeSet<_> = bootstrap::PRINCIPALS
        .iter()
        .map(|name| format!("{prefix}{name}"))
        .collect();
    if present.is_empty() && empty {
        return Ok(true);
    }
    if present != &expected || !canonical || (!empty && !adopted_verified) {
        return Err(refuse(
            "deployment bootstrap refuses an unknown database or mismatched/partial canonical role scope",
        ));
    }
    Ok(false)
}

fn deployment_admin_valid(
    current: &str,
    session: &str,
    database: &str,
    admin: bool,
    expected_user: &str,
    expected_database: &str,
) -> bool {
    admin && current == expected_user && session == expected_user && database == expected_database
}

fn deployment_password_missing(password: &str, stored: Option<&str>) -> Result<bool> {
    match stored {
        None => Ok(true),
        Some(value) if matches(password, value) => Ok(false),
        Some(_) => Err(refuse(
            "existing deployment credential does not match; no passwords rotated",
        )),
    }
}

/// Explicit administrator-only fresh deployment preparation. Uses the native
/// frozen role/extension primitives and initializes only absent SCRAM passwords.
pub async fn deployment_bootstrap(
    authority: &MigrationAuthority,
    baseline_id: &str,
    expected: &str,
    expected_admin: &str,
) -> Result<serde_json::Value> {
    tokio::time::timeout(
        Duration::from_secs(120),
        deployment_bootstrap_inner(authority, baseline_id, expected, expected_admin),
    )
    .await
    .map_err(|_| refuse("deployment bootstrap timed out; no credentials were rotated"))?
}

async fn deployment_bootstrap_inner(
    authority: &MigrationAuthority,
    baseline_id: &str,
    expected: &str,
    expected_admin: &str,
) -> Result<serde_json::Value> {
    let plan = deployment_plan(
        &crate::credential_files::bootstrap_values(
            &std::env::vars().collect(),
            &authority.role_prefix,
        )?,
        &authority.role_prefix,
        expected,
        expected_admin,
    )?;
    crate::commands::load_baseline(&authority.paths, baseline_id)?;
    let mut conn = tokio::time::timeout(Duration::from_secs(15), authority.connect(false))
        .await
        .map_err(|_| refuse("deployment bootstrap connection timed out"))??;
    sqlx::raw_sql("SET statement_timeout='30s'; SET lock_timeout='10s'")
        .execute(&mut conn)
        .await?;
    authority.acquire_lock(&mut conn).await?;
    let outcome = async {
        let state = sqlx::query("SELECT current_user::text AS identity,session_user::text AS session,current_database()::text AS database, (SELECT rolsuper FROM pg_roles WHERE rolname=current_user) AS admin,pg_get_userbyid(datdba)::text AS database_owner FROM pg_database WHERE datname=current_database()")
            .fetch_one(&mut conn).await?;
        if !deployment_admin_valid(&state.try_get::<String,_>("identity")?,
            &state.try_get::<String,_>("session")?, &state.try_get::<String,_>("database")?,
            state.try_get::<bool,_>("admin")?, &plan.admin_user, expected) {
            return Err(refuse("deployment bootstrap requires the explicitly named administrator login and database"));
        }
        let empty = bootstrap::database_empty(&mut conn, bootstrap::SCHEMAS).await?;
        let extra_schemas: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname !~ '^pg_' AND nspname <> 'information_schema' AND NOT (nspname = ANY($1)))")
            .bind(bootstrap::SCHEMAS).fetch_one(&mut conn).await?;
        if extra_schemas { return Err(refuse("deployment bootstrap refuses foreign database schemas")); }
        // Nonempty deployments are read-only fingerprint verified before any
        // credential update; an absent/mismatched adoption marker is refused.
        let adopted_verified = if !empty {
            let marker = authority.adopted_baseline(&mut conn).await?
                .ok_or_else(|| refuse("nonempty deployment requires a verified canonical adoption marker"))?;
            let (baseline, sha) = crate::commands::load_baseline(&authority.paths, baseline_id)?;
            bootstrap::validate_adoption_marker(&marker, &baseline, &sha)?;
            crate::commands::verify(authority, baseline_id).await?;
            true
        } else { false };
        let mut tx = conn.begin().await?;
        // Role/password check and initialization are atomic across cluster-wide
        // role updates, not only this database's migration advisory lock.
        sqlx::query("LOCK TABLE pg_catalog.pg_authid IN SHARE ROW EXCLUSIVE MODE").execute(&mut *tx).await?;
        let present: BTreeSet<String> = sqlx::query_scalar("SELECT rolname::text FROM pg_roles WHERE left(rolname,length($1))=$1 ORDER BY rolname")
            .bind(&authority.role_prefix).fetch_all(&mut *tx).await?.into_iter().collect();
        let canonical = if present.is_empty() { false } else {
            bootstrap::role_bootstrap_issues(&mut tx, &authority.role_prefix).await?.is_empty()
        };
        let provision = deployment_scope(&present, &authority.role_prefix, empty, canonical, adopted_verified)?;
        if provision {
            let canonical_schemas_present: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname=ANY(ARRAY['gateway','assistant','knowledge']))")
                .fetch_one(&mut *tx).await?;
            if state.try_get::<String,_>("database_owner")? != plan.admin_user || canonical_schemas_present {
                return Err(refuse("fresh deployment must be owned by the explicit bootstrap admin and have no preexisting application schemas"));
            }
            bootstrap::provision_roles_admin(&mut tx, &authority.paths, &authority.role_prefix).await?;
        }
        if empty { bootstrap::provision_extensions_admin(&mut tx, &authority.paths).await?; }
        let mut initialized = 0;
        let mut preserved = 0;
        for login in &plan.logins {
            let stored: Option<String> = sqlx::query_scalar("SELECT rolpassword FROM pg_catalog.pg_authid WHERE rolname=$1 AND rolcanlogin")
                .bind(&login.role).fetch_one(&mut *tx).await?;
            if !deployment_password_missing(&login.password, stored.as_deref())? {
                preserved += 1;
            } else {
                let encoded = verifier(&login.password, uuid::Uuid::new_v4().as_bytes());
                let statement: String = sqlx::query_scalar("SELECT pg_catalog.format('ALTER ROLE %I PASSWORD %L',$1::text,$2::text)")
                    .bind(&login.role).bind(encoded).fetch_one(&mut *tx).await?;
                sqlx::raw_sql(&statement).execute(&mut *tx).await?;
                initialized += 1;
            }
        }
        tx.commit().await?;
        let verified = tokio::time::timeout(Duration::from_secs(30), credentials::verify_role_connections(&authority.role_prefix, Some(expected))).await
            .map_err(|_| refuse("deployment role connection verification timed out"))??;
        Ok(serde_json::json!({"result":"pass","command":"deployment-bootstrap","initialized":initialized,"preserved":preserved,"verified_connections":verified,"passwords_rotated":false,"values_withheld":true}))
    }.await;
    let released = authority.release_lock(conn).await;
    let value = outcome?;
    released?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn deployment_values() -> BTreeMap<String, String> {
        credentials::ALL_PRINCIPALS
            .iter()
            .enumerate()
            .map(|(i, principal)| {
                let user = if *principal == "admin" {
                    "postgres".to_owned()
                } else {
                    format!("ai_gateway_{principal}")
                };
                (
                    format!("AI_GATEWAY_DATABASE_{}_DSN", principal.to_ascii_uppercase()),
                    format!("postgresql://{user}:fixture-{i:032}@postgres:5432/gateway"),
                )
            })
            .collect()
    }
    #[test]
    fn public_deployment_plan_accepts_named_endpoint_without_weakening_local_guard() {
        let values = deployment_values();
        assert_eq!(
            deployment_plan(&values, "ai_gateway_", "gateway", "postgres")
                .unwrap()
                .logins
                .len(),
            6
        );
        assert!(plan(&values, "ai_gateway_", "gateway").is_err());
        for principal in credentials::ALL_PRINCIPALS {
            let mut bad = values.clone();
            bad.remove(&format!(
                "AI_GATEWAY_DATABASE_{}_DSN",
                principal.to_ascii_uppercase()
            ));
            assert!(deployment_plan(&bad, "ai_gateway_", "gateway", "postgres").is_err());
        }
        let mut local = values.clone();
        local.insert("AI_PLATFORM_LOCAL_DEV_CREDENTIALS".into(), "true".into());
        assert!(deployment_plan(&local, "ai_gateway_", "gateway", "postgres").is_err());
        assert!(deployment_plan(&values, "ai_gateway_", "foreign", "postgres").is_err());
        assert!(deployment_plan(&values, "ai_gateway_", "gateway", "anotheradmin").is_err());
        assert!(deployment_plan(&values, "unsafe-prefix", "gateway", "postgres").is_err());
        assert!(
            deployment_plan(
                &values,
                "ai_gateway_",
                "gateway;DROP DATABASE x",
                "postgres"
            )
            .is_err()
        );
    }
    #[test]
    fn deployment_plan_rejects_redirect_shared_password_and_wrong_role_without_values() {
        let values = deployment_values();
        for replacement in [
            "postgresql://ai_gateway_runtime:private-sentinel-password-00000000@foreign/gateway",
            "postgresql://ai_gateway_runtime:private-sentinel-password-00000000@postgres/gateway?host=foreign",
            "postgresql://ai_gateway_runtime:private-sentinel-password-00000000@postgres/gateway?options=-crole=postgres",
            "postgresql://ai_gateway_gateway:private-sentinel-password-00000000@postgres/gateway",
            "postgresql://ai_gateway_runtime:private-sentinel-password-00000000@postgres/other",
            "postgresql://ai_gateway_runtime:private-sentinel-password-00000000@postgres/gateway#foreign",
            "postgresql://ai_gateway_runtime:short@postgres/gateway",
            "postgresql://ai_gateway_runtime:private-sentinel-password-00000000@postgres:5433/gateway",
        ] {
            let mut bad = values.clone();
            bad.insert("AI_GATEWAY_DATABASE_RUNTIME_DSN".into(), replacement.into());
            let error = deployment_plan(&bad, "ai_gateway_", "gateway", "postgres")
                .err()
                .unwrap()
                .to_string();
            assert!(!error.contains("private-sentinel") && !error.contains("postgresql://"));
        }
        let mut shared = values.clone();
        shared.insert(
            "AI_GATEWAY_DATABASE_RUNTIME_DSN".into(),
            values["AI_GATEWAY_DATABASE_GATEWAY_DSN"]
                .replace("ai_gateway_gateway", "ai_gateway_runtime"),
        );
        assert!(deployment_plan(&shared, "ai_gateway_", "gateway", "postgres").is_err());
    }
    #[test]
    fn deployment_scope_refuses_partial_foreign_and_unverified_business_databases() {
        let none = BTreeSet::new();
        let complete: BTreeSet<_> = bootstrap::PRINCIPALS
            .iter()
            .map(|p| format!("ai_gateway_{p}"))
            .collect();
        assert!(deployment_scope(&none, "ai_gateway_", true, false, false).unwrap());
        assert!(!deployment_scope(&complete, "ai_gateway_", true, true, false).unwrap());
        assert!(!deployment_scope(&complete, "ai_gateway_", false, true, true).unwrap());
        assert!(deployment_scope(&none, "ai_gateway_", false, false, false).is_err());
        assert!(deployment_scope(&complete, "ai_gateway_", false, true, false).is_err());
        assert!(deployment_scope(&complete, "ai_gateway_", true, false, false).is_err());
        let mut partial = complete.clone();
        partial.remove("ai_gateway_runtime");
        assert!(deployment_scope(&partial, "ai_gateway_", true, true, false).is_err());
        let mut foreign = complete;
        foreign.insert("ai_gateway_unrecognized".into());
        assert!(deployment_scope(&foreign, "ai_gateway_", true, true, false).is_err());
    }
    #[test]
    fn deployment_admin_must_be_explicit_superuser_session_and_database() {
        assert!(deployment_admin_valid(
            "postgres", "postgres", "gateway", true, "postgres", "gateway"
        ));
        for (current, session, database, admin) in [
            ("postgres", "other", "gateway", true),
            ("other", "other", "gateway", true),
            ("postgres", "postgres", "foreign", true),
            ("postgres", "postgres", "gateway", false),
        ] {
            assert!(!deployment_admin_valid(
                current, session, database, admin, "postgres", "gateway"
            ));
        }
    }
    #[test]
    fn deployment_passwords_initialize_null_only_and_never_rotate() {
        let password = "fixture-private-deployment-password";
        let encoded = verifier(password, &[7; 16]);
        assert!(deployment_password_missing(password, None).unwrap());
        assert!(!deployment_password_missing(password, Some(&encoded)).unwrap());
        for stored in [
            "md5legacy",
            "malformed",
            &verifier("other-password", &[7; 16]),
        ] {
            let error = deployment_password_missing(password, Some(stored))
                .unwrap_err()
                .to_string();
            assert!(!error.contains(password) && !error.contains(stored));
        }
    }

    #[test]
    fn compose_owner_config_running_state_and_loopback_port_are_all_required() {
        let root = std::path::Path::new("/fixture/repo");
        let valid = serde_json::json!({"project":"agentforge-dev-7801d4a9a4","working_dir":"/fixture/repo","config_files":"/fixture/repo/deploy/local/compose.yml","running":true,"ports":{"5432/tcp":[{"HostIp":"127.0.0.1","HostPort":"5432"}]}});
        assert!(validate_container(root, 5432, &valid).is_ok());
        for (key, replacement) in [
            ("project", serde_json::json!("other")),
            ("working_dir", serde_json::json!("/other/repo")),
            (
                "config_files",
                serde_json::json!("/fixture/repo/docker-compose.yml"),
            ),
            (
                "config_files",
                serde_json::json!(
                    "/fixture/repo/deploy/local/compose.yml,/uncontrolled/override.yml"
                ),
            ),
            ("running", serde_json::json!(false)),
            (
                "ports",
                serde_json::json!({"5432/tcp":[{"HostIp":"0.0.0.0","HostPort":"5432"}]}),
            ),
            (
                "ports",
                serde_json::json!({"5432/tcp":[{"HostIp":"::1","HostPort":"5432"}]}),
            ),
        ] {
            let mut broken = valid.clone();
            broken[key] = replacement;
            assert!(validate_container(root, 5432, &broken).is_err(), "{key}");
        }
        assert!(validate_container(root, 6543, &valid).is_err());
        let mut overlay = valid.clone();
        overlay["config_files"] = serde_json::json!(
            "/fixture/repo/deploy/local/compose.yml,/fixture/repo/deploy/local/compose.runtime.yml"
        );
        assert!(validate_container(root, 5432, &overlay).is_ok());
    }
    #[test]
    fn scram_roundtrip_rejects_wrong_password_and_malformed_verifiers() {
        // Independent hashlib.pbkdf2_hmac + hmac/SHA-256 fixture; not our verifier's own output.
        let golden_salt = STANDARD.decode("W22ZaJ0SNY7soEsUEjb6gQ==").unwrap();
        assert_eq!(
            verifier("pencil", &golden_salt),
            "SCRAM-SHA-256$4096:W22ZaJ0SNY7soEsUEjb6gQ==$WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU="
        );
        let encoded = verifier("a-password-only-for-this-fixture", &[7; 16]);
        assert!(matches("a-password-only-for-this-fixture", &encoded));
        assert!(!matches("another-password", &encoded));
        for invalid in [
            "md5legacy",
            "SCRAM-SHA-256$0:AA==$AA:AA",
            "SCRAM-SHA-256$999999999:AA==$AA:AA",
            "SCRAM-SHA-256$4096:!!!$AA:AA",
        ] {
            assert!(!matches("fixture", invalid));
        }
        assert!(!encoded.contains("a-password-only-for-this-fixture"));
    }
    #[test]
    fn credential_plan_rejects_shared_remote_and_redirected_targets_without_values() {
        let mut values = BTreeMap::new();
        for (i, p) in credentials::ALL_PRINCIPALS.iter().enumerate() {
            let user = if *p == "admin" {
                "postgres".into()
            } else {
                format!("ai_gateway_{p}")
            };
            values.insert(
                format!("AI_GATEWAY_DATABASE_{}_DSN", p.to_ascii_uppercase()),
                format!("postgresql://{user}:fixture-{i:032}@127.0.0.1:5432/gateway"),
            );
        }
        assert_eq!(
            plan(&values, "ai_gateway_", "gateway")
                .unwrap()
                .logins
                .len(),
            6
        );
        for replacement in [
            "postgresql://ai_gateway_runtime:fixture-secret-that-must-never-appear@production.invalid/gateway",
            "postgresql://ai_gateway_runtime:fixture-secret-that-must-never-appear@127.0.0.1/gateway?host=production.invalid",
            "postgresql://ai_gateway_gateway:fixture-secret-that-must-never-appear@127.0.0.1/gateway",
            "postgresql://ai_gateway_runtime:fixture-secret-that-must-never-appear@localhost/gateway",
            "postgresql://ai_gateway_runtime:fixture-secret-that-must-never-appear@[::1]/gateway",
        ] {
            let mut broken = values.clone();
            broken.insert("AI_GATEWAY_DATABASE_RUNTIME_DSN".into(), replacement.into());
            let error = plan(&broken, "ai_gateway_", "gateway")
                .err()
                .unwrap()
                .to_string();
            assert!(!error.contains("fixture-secret"));
            assert!(!error.contains("postgresql://"));
        }
        assert!(plan(&values, "ai_gateway_", "unrelated").is_err());
        assert!(plan(&values, "unsafe-prefix", "gateway").is_err());
    }
}
