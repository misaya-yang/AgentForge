//! Explicit first-account provisioning after the fresh schema authority succeeds.
//! Only the existing Gateway business role writes; schema/roles/credentials stay
//! under their original authorities. Input, hashes and driver errors are private.
use ai_platform_service_primitives::data_connection::{
    ConnectionConfig, ConnectionPurpose, DatabaseRole,
};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeMap, io::Read, time::Duration};

const DATABASE: &str = "gateway_dev_fresh";
const MAX_INPUT: usize = 16 * 1024;
type Result<T> = std::result::Result<T, &'static str>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    email: String,
    password: String,
}

impl Input {
    fn parse(bytes: &[u8], domain: &str) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_INPUT {
            return Err("admin bootstrap input size invalid");
        }
        let mut value: Self =
            serde_json::from_slice(bytes).map_err(|_| "admin bootstrap input invalid")?;
        value.email = ai_platform_auth::normalize_email(&value.email, domain)
            .map_err(|_| "admin bootstrap email invalid")?;
        // The initial credential is still subject to the ordinary account policy;
        // provisioning does not disable the first-login password-change gate.
        ai_platform_auth::validate_password_change(
            "bootstrap-not-a-credential",
            &value.password,
            &value.password,
        )
        .map_err(|_| "admin bootstrap password policy failed")?;
        Ok(value)
    }
}

fn configuration(environment: &BTreeMap<String, String>) -> Result<ConnectionConfig> {
    if [
        "DATABASE_URL",
        "PGPASSWORD",
        "POSTGRES_PASSWORD",
        "ADMIN_PASSWORD",
        "ADMIN_EMAIL",
    ]
    .iter()
    .any(|key| environment.get(*key).is_some_and(|v| !v.is_empty()))
    {
        return Err("fresh admin bootstrap refuses inline legacy credentials");
    }
    let mut config = ConnectionConfig::from_environment(
        "DATA_GATEWAY",
        DatabaseRole::Gateway,
        ConnectionPurpose::BackendPinned,
        environment,
    )
    .map_err(|_| "fresh admin database configuration invalid")?;
    if config.physical_database != DATABASE || config.expected_database != DATABASE {
        return Err("fresh admin bootstrap requires gateway_dev_fresh");
    }
    config.max_connections = 1;
    Ok(config)
}

pub(super) async fn run() -> Result<()> {
    let environment = std::env::vars().collect::<BTreeMap<_, _>>();
    let config = configuration(&environment)?;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take((MAX_INPUT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "admin bootstrap input unavailable")?;
    let input = Input::parse(
        &bytes,
        environment
            .get("AUTH_ALLOWED_EMAIL_DOMAIN")
            .map(String::as_str)
            .unwrap_or("example.com"),
    )?;
    let hash = ai_platform_auth::PasswordEngine::new(1)
        .map_err(|_| "admin password engine unavailable")?
        .hash(input.password)
        .await
        .map_err(|_| "admin password hashing failed")?;
    let pool = config
        .connect()
        .await
        .map_err(|_| "fresh admin database connection failed")?;
    let outcome = tokio::time::timeout(
        Duration::from_secs(45),
        initialize(&pool, &input.email, &hash),
    )
    .await
    .map_err(|_| "fresh admin initialization deadline exceeded")
    .and_then(|v| v);
    // Drop/rollback owned work before returning. No database/secret is printed.
    let _ = tokio::time::timeout(Duration::from_secs(3), pool.close()).await;
    match outcome? {
        Outcome::Created => {
            println!("Fresh administrator created; first-login password change required");
        }
        Outcome::Existing => {
            println!("Existing fresh administrator verified; credentials unchanged")
        }
    }
    Ok(())
}

enum Outcome {
    Created,
    Existing,
}

async fn initialize(pool: &sqlx::PgPool, email: &str, hash: &str) -> Result<Outcome> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| "admin transaction unavailable")?;
    sqlx::query("SET LOCAL statement_timeout = '10000'")
        .execute(&mut *tx)
        .await
        .map_err(|_| "admin transaction configuration failed")?;
    sqlx::query("SET LOCAL lock_timeout = '2000'")
        .execute(&mut *tx)
        .await
        .map_err(|_| "admin transaction configuration failed")?;
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtextextended('agentforge:fresh-admin-bootstrap:v1',0))")
        .execute(&mut *tx).await.map_err(|_| "admin bootstrap already in progress")?;
    let schema: Value = sqlx::query_scalar("SELECT gateway.platform_fresh_schema_snapshot_v1()")
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| "fresh schema admission unavailable")?;
    if schema["schema_version"] != "agentforge/fresh-schema-admission/v1"
        || schema["mode"] != "fresh-development"
        || schema["catalog_matches"] != true
        || schema["transactional_initialization"] != true
        || schema["historical_upgrade_acceptance"] != false
        || schema["database"] != DATABASE
    {
        return Err("fresh schema admission rejected admin bootstrap");
    }
    let occupied: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway.users)")
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| "existing account lookup failed")?;
    if occupied {
        let existing: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway.users u JOIN gateway.tenants t USING(tenant_id) JOIN gateway.user_roles ur USING(user_id) JOIN gateway.rbac_roles r ON r.role_name=ur.role_name JOIN gateway.role_permissions p ON p.role_name=r.role_name WHERE u.user_id='admin' AND u.tenant_id='default' AND u.email=$1 AND u.status='active' AND u.password_hash IS NOT NULL AND u.password_hash<>'' AND t.status='active' AND ur.role_name='admin' AND (ur.expires_at IS NULL OR ur.expires_at>NOW()) AND r.is_system AND p.permission_code='admin:*')")
            .bind(email).fetch_one(&mut *tx).await.map_err(|_| "existing admin verification failed")?;
        if !existing {
            return Err("existing accounts conflict with bootstrap; reset or promotion refused");
        }
        tx.commit()
            .await
            .map_err(|_| "admin verification transaction failed")?;
        return Ok(Outcome::Existing);
    }
    let tenant: Option<String> = sqlx::query_scalar(
        "SELECT status FROM gateway.tenants WHERE tenant_id='default' FOR UPDATE",
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| "bootstrap tenant lookup failed")?;
    match tenant.as_deref() {
        None => {
            sqlx::query("INSERT INTO gateway.tenants(tenant_id,name,status) VALUES('default','Default','active')")
                .execute(&mut *tx).await.map_err(|_| "bootstrap tenant creation failed")?;
        }
        Some("active") => {}
        _ => return Err("existing default tenant is inactive; bootstrap refuses change"),
    }
    let role: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway.rbac_roles r JOIN gateway.role_permissions p USING(role_name) WHERE r.role_name='admin' AND r.is_system AND p.permission_code='admin:*')")
        .fetch_one(&mut *tx).await.map_err(|_| "bootstrap role lookup failed")?;
    if !role {
        return Err("canonical admin role unavailable");
    }
    sqlx::query("INSERT INTO gateway.users(user_id,username,email,display_name,tenant_id,tier,roles,status,password_hash,force_password_change,email_verified,created_by) VALUES('admin','admin',$1,'System Administrator','default','admin',ARRAY['admin']::varchar(50)[],'active',$2,true,true,'system')")
        .bind(email).bind(hash).execute(&mut *tx).await.map_err(|_| "admin creation failed")?;
    sqlx::query("INSERT INTO gateway.user_roles(user_id,role_name,granted_by) VALUES('admin','admin','system')")
        .execute(&mut *tx).await.map_err(|_| "current admin role binding failed")?;
    tx.commit()
        .await
        .map_err(|_| "admin commit outcome requires reconciliation")?;
    Ok(Outcome::Created)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_uses_normal_email_and_password_policy() {
        let input = Input::parse(
            br#"{"email":" ADMIN@EXAMPLE.COM ","password":"Fresh1!secret"}"#,
            "example.com",
        )
        .unwrap();
        assert_eq!(input.email, "admin@example.com");
        for bytes in [
            br#"{"email":"admin@foreign.example","password":"Fresh1!secret"}"#.as_slice(),
            br#"{"email":"admin@example.com","password":"weak"}"#,
            br#"{"email":"admin@example.com","password":"Fresh1!secret","force_password_change":false}"#,
            br#"{"email":"admin@example.com","email":"other@example.com","password":"Fresh1!secret"}"#,
        ] {
            assert!(Input::parse(bytes, "example.com").is_err());
        }
    }

    #[test]
    fn oversized_or_secret_bearing_invalid_inputs_have_fixed_errors() {
        assert!(Input::parse(&vec![b'a'; MAX_INPUT + 1], "example.com").is_err());
        let message = Input::parse(
            br#"{"password":"never-print-this-private-value"}"#,
            "example.com",
        )
        .err()
        .unwrap();
        assert!(!message.contains("never-print"));
        assert!(
            configuration(&BTreeMap::from([(
                "DATABASE_URL".into(),
                "never-print-dsn".into()
            )]))
            .err()
            .is_some()
        );
    }

    #[test]
    fn fresh_bootstrap_refuses_other_databases_and_transaction_aliases() {
        let mut values = BTreeMap::from([
            ("DATA_GATEWAY_MODE".into(), "direct".into()),
            ("DATA_GATEWAY_DSN_FILE".into(), "/unused/dsn".into()),
            ("DATA_GATEWAY_TLS_CA_FILE".into(), "/unused/ca".into()),
            ("DATA_GATEWAY_TLS_CERT_FILE".into(), "/unused/cert".into()),
            ("DATA_GATEWAY_TLS_KEY_FILE".into(), "/unused/key".into()),
            ("DATA_GATEWAY_EXPECTED_HOST".into(), "postgres".into()),
            ("DATA_GATEWAY_DATABASE_ALIAS".into(), DATABASE.into()),
            ("DATA_GATEWAY_PHYSICAL_DATABASE".into(), DATABASE.into()),
        ]);
        assert_eq!(configuration(&values).unwrap().max_connections, 1);
        values.insert("DATA_GATEWAY_PHYSICAL_DATABASE".into(), "gateway".into());
        assert!(configuration(&values).is_err());
        values.insert("DATA_GATEWAY_PHYSICAL_DATABASE".into(), DATABASE.into());
        values.insert("DATA_GATEWAY_DATABASE_ALIAS".into(), "gateway".into());
        assert!(configuration(&values).is_err());
        values.insert("DATA_GATEWAY_DATABASE_ALIAS".into(), DATABASE.into());
        values.insert("DATA_GATEWAY_MODE".into(), "transaction".into());
        assert!(configuration(&values).is_err());
    }
}
