//! Read-only secure deployment admission. No administrator connection, provisioning,
//! credential initialization, schema creation or migration is performed here.
use crate::{
    AuthorityError, Result,
    application_guard::{self, ApplicationPrincipal},
    bootstrap, commands, credential_files, credentials, manifest,
    runner::{AuthorityPaths, MigrationAuthority},
};
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, Row};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    PreMigration,
    PostMigration,
}
impl Phase {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "pre-migration" => Ok(Self::PreMigration),
            "post-migration" => Ok(Self::PostMigration),
            _ => Err(refused("unknown admission phase")),
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::PreMigration => "pre-migration",
            Self::PostMigration => "post-migration",
        }
    }
}
fn refused(s: &str) -> AuthorityError {
    AuthorityError::Refused(format!("deployment file admission {s}; values withheld"))
}
fn state(
    phase: Phase,
    empty: bool,
    adopted: bool,
    installed: i32,
    source: i32,
    attempts: bool,
) -> Result<&'static str> {
    if source < 1 || installed < 0 || installed > source || attempts {
        return Err(refused("foreign/future/incomplete schema state"));
    }
    if empty && !adopted && installed == 0 && phase == Phase::PreMigration {
        return Ok("operator_prepared_empty");
    }
    if !empty && adopted && (phase == Phase::PreMigration || installed == source) {
        return Ok(if phase == Phase::PreMigration {
            "verified_adopted_prefix"
        } else {
            "admitted_current_source"
        });
    }
    Err(refused(
        "schema state requires a separate operator authority or migration stage",
    ))
}
fn files(
    e: &BTreeMap<String, String>,
    prefix: &str,
    expected: &str,
) -> Result<BTreeMap<String, credential_files::PrincipalConnection>> {
    if expected.is_empty() || expected.chars().any(char::is_control) {
        return Err(refused("explicit expected database required"));
    }
    let mut connections = BTreeMap::new();
    let mut keys = BTreeSet::new();
    let mut passwords = BTreeSet::new();
    let mut endpoint = None;
    for principal in
        std::iter::once("migrator").chain(credentials::APPLICATION_PRINCIPALS.iter().copied())
    {
        let c = credential_files::resolve(e, principal, prefix)?;
        if c.password_identity
            .as_ref()
            .is_some_and(|p| !passwords.insert(p.clone()))
        {
            return Err(refused("principals share a password"));
        }
        let key = c
            .tls_key_identity
            .as_ref()
            .ok_or_else(|| refused("all six principals require file credentials and mTLS"))?;
        if !keys.insert(key.clone()) {
            return Err(refused("principals share a TLS client key"));
        }
        let url = url::Url::parse(&c.dsn).map_err(|_| refused("file DSN invalid"))?;
        let target = (
            url.host_str().unwrap_or("").to_owned(),
            url.port().unwrap_or(0),
        );
        if c.options.get_database() != Some(expected)
            || endpoint.as_ref().is_some_and(|old| old != &target)
        {
            return Err(refused(
                "principals target different endpoints or databases",
            ));
        }
        endpoint = Some(target);
        connections.insert(principal.into(), c);
    }
    Ok(connections)
}
async fn connect(c: &credential_files::PrincipalConnection) -> Result<PgConnection> {
    let mut conn = tokio::time::timeout(
        Duration::from_secs(10),
        PgConnection::connect_with(&c.options),
    )
    .await
    .map_err(|_| refused("connection timed out"))?
    .map_err(|_| refused("connection failed"))?;
    sqlx::raw_sql(
        "SET default_transaction_read_only=on; SET statement_timeout='5s'; SET lock_timeout='5s'",
    )
    .execute(&mut conn)
    .await
    .map_err(|_| refused("read-only session setup failed"))?;
    Ok(conn)
}
async fn identity(
    conn: &mut PgConnection,
    prefix: &str,
    principal: &str,
    expected: &str,
) -> Result<()> {
    let row=sqlx::query("SELECT current_user::text AS identity,session_user::text AS session,current_database()::text AS database,r.rolcanlogin, r.rolsuper OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication OR r.rolbypassrls OR r.rolinherit AS privileged,has_table_privilege(current_user,'pg_catalog.pg_authid','SELECT') AS secret_catalog FROM pg_catalog.pg_roles r WHERE r.rolname=current_user")
        .fetch_one(&mut *conn).await.map_err(|_|refused("principal probe failed"))?;
    if row.try_get::<String, _>("identity")? != format!("{prefix}{principal}")
        || row.try_get::<String, _>("session")? != format!("{prefix}{principal}")
        || row.try_get::<String, _>("database")? != expected
        || !row.try_get::<bool, _>("rolcanlogin")?
        || row.try_get::<bool, _>("privileged")?
        || row.try_get::<bool, _>("secret_catalog")?
    {
        return Err(refused(
            "unprivileged dedicated login or target database mismatch",
        ));
    }
    let tls:bool=sqlx::query_scalar("SELECT COALESCE(bool_and(ssl AND client_dn IS NOT NULL AND length(client_dn)>0 AND client_serial IS NOT NULL),false) FROM pg_catalog.pg_stat_ssl WHERE pid=pg_backend_pid()")
        .fetch_one(&mut *conn).await.map_err(|_|refused("mTLS session evidence unavailable"))?;
    if !tls {
        return Err(refused(
            "server did not authenticate this TLS client certificate",
        ));
    }
    if principal != "migrator" {
        let forbidden:bool=sqlx::query_scalar("SELECT pg_has_role(current_user,$1,'MEMBER') OR EXISTS(SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relname IN ('platform_schema_baselines','platform_schema_changes','platform_schema_change_attempts') AND has_table_privilege(current_user,c.oid,'SELECT,INSERT,UPDATE,DELETE'))")
            .bind(format!("{prefix}owner")).fetch_one(&mut *conn).await?;
        if forbidden {
            return Err(refused("application gained owner/ledger authority"));
        }
    }
    Ok(())
}
fn profile(principal: &str) -> Result<ApplicationPrincipal> {
    match principal {
        "gateway" => Ok(ApplicationPrincipal::GatewayDurable),
        "runtime" => Ok(ApplicationPrincipal::Runtime),
        "capability_worker" => Ok(ApplicationPrincipal::CapabilityWorker),
        "knowledge_api" => Ok(ApplicationPrincipal::KnowledgeApi),
        "knowledge_worker" => Ok(ApplicationPrincipal::KnowledgeWorker),
        _ => Err(refused("unknown application identity")),
    }
}
async fn schema(
    authority: &MigrationAuthority,
    conn: &mut PgConnection,
    baseline_id: &str,
    phase: Phase,
) -> Result<(i32, i32, &'static str)> {
    let (baseline, sha) = commands::load_baseline(&authority.paths, baseline_id)?;
    let source = manifest::load_epoch_manifest(
        &authority.paths.epoch_dir(baseline_id).join("manifest.yml"),
    )?;
    let ledger_count:i64=sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relkind='r' AND c.relname IN ('platform_schema_baselines','platform_schema_changes','platform_schema_change_attempts')").fetch_one(&mut *conn).await?;
    let empty = bootstrap::database_empty(conn, bootstrap::SCHEMAS).await?;
    if ledger_count == 0 && empty {
        let default_grants:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_default_acl d CROSS JOIN LATERAL pg_catalog.aclexplode(d.defaclacl) a WHERE d.defaclrole=(SELECT oid FROM pg_catalog.pg_roles WHERE rolname=$1) AND a.grantee<>d.defaclrole)").bind(format!("{}owner",authority.role_prefix)).fetch_one(&mut *conn).await?;
        if default_grants {
            return Err(refused("empty operator prep has non-owner default grants"));
        }
        let extensions = crate::fingerprint::extensions_lines(conn, &authority.role_prefix).await?;
        if crate::fingerprint::canonical_digest(&extensions) != baseline.extensions_sha256 {
            return Err(refused(
                "operator extension identities differ from frozen source",
            ));
        }
        return Ok((
            0,
            source.epoch,
            state(phase, true, false, 0, source.epoch, false)?,
        ));
    }
    if ledger_count != 3 || !crate::ledger_acl::inspect(conn, &authority.role_prefix).await? {
        return Err(refused("partial or noncanonical authority ledger"));
    }
    let marker = authority
        .adopted_baseline(conn)
        .await?
        .ok_or_else(|| refused("nonempty database has no adopted canonical baseline"))?;
    bootstrap::validate_adoption_marker(&marker, &baseline, &sha)?;
    let attempts: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM public.platform_schema_change_attempts)")
            .fetch_one(&mut *conn)
            .await?;
    let foreign: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM public.platform_schema_changes WHERE baseline_id<>$1)",
    )
    .bind(baseline_id)
    .fetch_one(&mut *conn)
    .await?;
    if foreign {
        return Err(refused("foreign ledger rows"));
    }
    let applied = authority.applied_changes(conn, baseline_id).await?;
    crate::runner::validate_applied_prefix(&source, &applied)?;
    let rows=sqlx::query("SELECT sequence,name FROM public.platform_schema_changes WHERE baseline_id=$1 ORDER BY sequence").bind(baseline_id).fetch_all(&mut *conn).await?;
    for row in rows {
        let sequence: i32 = row.try_get("sequence")?;
        if source
            .changes
            .get((sequence - 1) as usize)
            .is_none_or(|c| Some(c.name.as_str()) != row.try_get::<&str, _>("name").ok())
        {
            return Err(refused("ledger name differs from exact source prefix"));
        }
    }
    let installed = applied.keys().last().copied().unwrap_or(0);
    let status = state(phase, false, true, installed, source.epoch, attempts)?;
    commands::require_ledger_state(authority, conn, &baseline).await?;
    // Reuse every frozen and exact in-memory superseded postcondition. This is
    // a readonly owner-capability check on the migrator, never an app fallback.
    commands::verify_for_admission(authority, baseline_id).await?;
    Ok((installed, source.epoch, status))
}
pub async fn run(
    paths: AuthorityPaths,
    prefix: String,
    baseline_id: &str,
    expected: &str,
    phase: Phase,
) -> Result<Value> {
    let e = std::env::vars().collect();
    let connections = files(&e, &prefix, expected)?;
    let migrator = connections
        .get("migrator")
        .ok_or_else(|| refused("migrator files absent"))?;
    let authority = MigrationAuthority::with_connect_options(
        migrator.dsn.clone(),
        migrator.options.clone(),
        paths,
        prefix.clone(),
    )?;
    let mut conn = connect(migrator).await?;
    let result=async {
        identity(&mut conn,&prefix,"migrator",expected).await?;
        let acquired:bool=sqlx::query_scalar("SELECT pg_try_advisory_lock($1::integer,$2::integer)").bind(crate::LOCK_NAMESPACE).bind(crate::LOCK_ID).fetch_one(&mut conn).await?;
        if !acquired {return Err(refused("migration authority is busy"));}
        if !bootstrap::role_bootstrap_issues(&mut conn,&prefix).await?.is_empty() {return Err(refused("operator role/schema/default ACL prep is incomplete"));}
        let safe_membership:bool=sqlx::query_scalar("SELECT count(*)=1 AND COALESCE(bool_and(NOT m.admin_option AND NOT m.inherit_option AND m.set_option AND m.roleid=(SELECT oid FROM pg_catalog.pg_roles WHERE rolname=$1)),false) FROM pg_catalog.pg_auth_members m WHERE m.member=(SELECT oid FROM pg_catalog.pg_roles WHERE rolname=current_user)").bind(format!("{prefix}owner")).fetch_one(&mut conn).await?;
        if !safe_membership {return Err(refused("migrator owner membership options differ from least authority"));}
        let (installed,source,status)=schema(&authority,&mut conn,baseline_id,phase).await?;
        for principal in credentials::APPLICATION_PRINCIPALS {
            let c=connections.get(*principal).ok_or_else(||refused("application files absent"))?;let mut app=connect(c).await?;
            identity(&mut app,&prefix,principal,expected).await?;
            if phase==Phase::PostMigration {application_guard::require_compatible_schema(&mut app,profile(principal)?).await?;}
            for sql in crate::source_tools::service_successors::admission_checks(installed,principal)? {
                let allowed:bool=sqlx::query_scalar(&sql).fetch_one(&mut app).await.map_err(|_|refused("service schema privilege probe failed"))?;
                if !allowed {return Err(refused("service schema minimum/forbidden privileges differ"));}
            }
            app.close().await.map_err(|_|refused("application connection close failed"))?;
        }
        Ok(json!({"result":"pass","command":"deployment-file-admission","phase":phase.name(),"schema_state":status,"source_epoch":source,"installed_epoch":installed,"migration_required":installed<source,"business_schema_admitted":phase==Phase::PostMigration,"verified_principals":6,"mTLS_client_auth_verified":true,"operator_bootstrap_performed":false,"admin_connection_used":false,"passwords_rotated":false,"database_mutations":false,"values_withheld":true}))
    }.await;
    // Closing the only lock-owning session releases its advisory lock on every path.
    let closed = tokio::time::timeout(Duration::from_secs(2), conn.close()).await;
    let value = result?;
    closed
        .map_err(|_| refused("migrator close timed out"))?
        .map_err(|_| refused("migrator close failed"))?;
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pre_migration_never_claims_application_or_fresh_bootstrap_acceptance() {
        assert_eq!(
            state(Phase::PreMigration, true, false, 0, 29, false).unwrap(),
            "operator_prepared_empty"
        );
        assert_eq!(
            state(Phase::PreMigration, false, true, 25, 29, false).unwrap(),
            "verified_adopted_prefix"
        );
        assert!(state(Phase::PostMigration, true, false, 0, 29, false).is_err());
        assert!(state(Phase::PostMigration, false, true, 25, 29, false).is_err());
        assert_eq!(
            state(Phase::PostMigration, false, true, 29, 29, false).unwrap(),
            "admitted_current_source"
        );
        for (empty, adopted, installed, attempts) in [
            (false, false, 0, false),
            (false, true, 30, false),
            (false, true, 29, true),
            (true, true, 0, false),
        ] {
            assert!(state(Phase::PreMigration, empty, adopted, installed, 29, attempts).is_err());
        }
    }
    #[test]
    fn files_only_six_principals_no_admin_fallback() {
        let e = BTreeMap::from([
            (
                "AI_GATEWAY_DATABASE_ADMIN_DSN".into(),
                "private-admin-sentinel".into(),
            ),
            (
                "AI_GATEWAY_DATABASE_MIGRATOR_DSN".into(),
                "postgresql://ai_gateway_migrator:private-sentinel@postgres:5432/gateway".into(),
            ),
        ]);
        let error = files(&e, "ai_gateway_", "gateway").unwrap_err().to_string();
        assert!(!error.contains("private-sentinel"));
        assert!(error.contains("all six principals require file credentials"));
        assert!(Phase::parse("bootstrap").is_err());
        assert!(Phase::parse("post-migration").is_ok());
    }
}

#[cfg(test)]
mod file_policy_tests {
    use super::*;
    struct Private(std::path::PathBuf);
    impl Drop for Private {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    impl Private {
        fn file(&self, name: &str, value: &str) -> String {
            let path = self.0.join(name);
            std::fs::write(&path, value).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            path.to_string_lossy().into_owned()
        }
        fn fixture() -> (Self, BTreeMap<String, String>) {
            let root =
                std::env::temp_dir().join(format!("file-admission-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            let files = Self(root);
            let mut e = BTreeMap::new();
            for principal in std::iter::once("migrator")
                .chain(credentials::APPLICATION_PRINCIPALS.iter().copied())
            {
                let prefix = format!("AI_GATEWAY_DATABASE_{}", principal.to_ascii_uppercase());
                for (key,value) in [("DSN_FILE",files.file(&format!("{principal}.dsn"),&format!("postgresql://fixture_{principal}@postgres:5432/gateway"))),("PASSWORD_FILE",files.file(&format!("{principal}.password"),&format!("file-only-password-{principal}-long-fixture"))),("TLS_CA_FILE",files.file(&format!("{principal}.ca"),"-----BEGIN CERTIFICATE-----\nCA\n-----END CERTIFICATE-----")),("TLS_CERT_FILE",files.file(&format!("{principal}.cert"),&format!("-----BEGIN CERTIFICATE-----\n{principal}\n-----END CERTIFICATE-----"))),("TLS_KEY_FILE",files.file(&format!("{principal}.key"),&format!("-----BEGIN PRIVATE KEY-----\n{principal}\n-----END PRIVATE KEY-----"))),("EXPECTED_HOST","postgres".into()),("DATABASE_ALIAS","gateway".into()),("PHYSICAL_DATABASE","gateway".into())] {e.insert(format!("{prefix}_{key}"),value);}
            }
            (files, e)
        }
    }
    #[test]
    fn six_role_file_preflight_does_not_read_an_admin_secret() {
        let (_files, mut e) = Private::fixture();
        e.insert(
            "AI_GATEWAY_DATABASE_ADMIN_DSN_FILE".into(),
            "/must-not-read/admin-secret".into(),
        );
        let plan = files(&e, "fixture_", "gateway").unwrap();
        assert_eq!(plan.len(), 6);
        assert!(!plan.contains_key("admin"));
        assert!(files(&e, "fixture_", "other").is_err());
    }
    #[test]
    fn cross_role_password_key_and_endpoint_sharing_are_refused() {
        let (_files, mut e) = Private::fixture();
        e.insert(
            "AI_GATEWAY_DATABASE_RUNTIME_PASSWORD_FILE".into(),
            e["AI_GATEWAY_DATABASE_GATEWAY_PASSWORD_FILE"].clone(),
        );
        assert!(files(&e, "fixture_", "gateway").is_err());
        let (_files, mut e) = Private::fixture();
        e.insert(
            "AI_GATEWAY_DATABASE_RUNTIME_TLS_KEY_FILE".into(),
            e["AI_GATEWAY_DATABASE_GATEWAY_TLS_KEY_FILE"].clone(),
        );
        assert!(files(&e, "fixture_", "gateway").is_err());
        let (owned, mut e) = Private::fixture();
        e.insert(
            "AI_GATEWAY_DATABASE_RUNTIME_TLS_KEY_FILE".into(),
            owned.file(
                "rewrapped.key",
                "-----BEGIN PRIVATE KEY-----\r\n gate way \r\n-----END PRIVATE KEY-----\r\n",
            ),
        );
        assert!(files(&e, "fixture_", "gateway").is_err());
        let (owned, mut e) = Private::fixture();
        e.insert(
            "AI_GATEWAY_DATABASE_RUNTIME_DSN_FILE".into(),
            owned.file(
                "foreign.dsn",
                "postgresql://fixture_runtime@foreign:5432/gateway",
            ),
        );
        e.insert(
            "AI_GATEWAY_DATABASE_RUNTIME_EXPECTED_HOST".into(),
            "foreign".into(),
        );
        assert!(files(&e, "fixture_", "gateway").is_err());
    }
}
