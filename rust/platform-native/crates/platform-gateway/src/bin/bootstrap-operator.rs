//! Explicit installation command; never invoked by normal Gateway startup.
//! Secret input is bounded JSON on non-terminal stdin, never command arguments.
use ai_platform_auth::{PasswordEngine, normalize_email, validate_password_change};
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;
use std::io::{self, IsTerminal, Read};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    database_url: String,
    expected_database: String,
    email: String,
    allowed_email_domain: String,
    password: String,
}

async fn run() -> Result<(), &'static str> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args != ["--create"] || io::stdin().is_terminal() {
        return Err("Usage: bootstrap-operator --create; supply private JSON on non-terminal stdin");
    }
    let mut bytes = Vec::new();
    io::stdin()
        .take(16 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read operator input")?;
    if bytes.len() > 16 * 1024 {
        return Err("Operator input is too large");
    }
    let input: Input = serde_json::from_slice(&bytes).map_err(|_| "Invalid operator input")?;
    // Do not retain a second plaintext copy after parsing.
    bytes.fill(0);
    if input.expected_database.is_empty() || !input.email.contains('@') {
        return Err("Explicit database name and full operator email are required");
    }
    let email = normalize_email(&input.email, &input.allowed_email_domain)
        .map_err(|_| "Invalid operator email or configured domain")?;
    // Reuse the canonical new-password policy. This sentinel is not a password
    // or fallback; no valid new password can equal the empty-policy sentinel.
    validate_password_change("\0", &input.password, &input.password)
        .map_err(|_| "Operator password does not meet policy")?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&input.database_url)
        .await
        .map_err(|_| "Could not connect to the specified database")?;
    let actual: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .map_err(|_| "Could not verify database identity")?;
    if actual != input.expected_database {
        return Err("Database name does not match the explicit guard");
    }
    let mut tx = pool.begin().await.map_err(|_| "Could not begin operator creation")?;
    sqlx::query("SELECT pg_advisory_xact_lock(1769104756, 1869636978)")
        .execute(&mut *tx)
        .await
        .map_err(|_| "Could not acquire operator creation lock")?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM gateway.users WHERE user_id='admin' OR LOWER(email)=LOWER($1) OR 'platform_admin'=ANY(roles))",
    )
    .bind(&email)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| "Could not check existing operator identities")?;
    if exists {
        return Err("Operator or email already exists; no account or password was changed");
    }
    let roles: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM gateway.rbac_roles WHERE role_name IN ('admin','platform_admin') AND is_system=TRUE",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| "Could not verify installed roles")?;
    if roles != 2 {
        return Err("Required system roles are missing; complete canonical installation first");
    }
    let passwords = PasswordEngine::new(1).map_err(|_| "Could not initialize password engine")?;
    let hash = passwords.hash(input.password).await.map_err(|_| "Could not hash operator password")?;
    // Tenant and operator are application data, excluded from frozen baseline.
    // Never reactivate or otherwise modify a pre-existing tenant.
    sqlx::query(
        "INSERT INTO gateway.tenants (tenant_id,name,description,tier,status,metadata) VALUES ('default','Default','System default tenant','normal','active','{}'::jsonb) ON CONFLICT (tenant_id) DO NOTHING",
    )
    .execute(&mut *tx)
    .await
    .map_err(|_| "Could not initialize default tenant")?;
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM gateway.tenants WHERE tenant_id='default' AND status='active')",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| "Could not verify default tenant")?;
    if !active {
        return Err("Existing default tenant is inactive; no operator was created");
    }
    sqlx::query(
        "INSERT INTO gateway.users (user_id,username,email,display_name,tenant_id,tier,roles,permissions,status,password_hash,force_password_change,email_verified,created_by) VALUES ('admin','admin',$1,'System Administrator','default','admin',ARRAY['admin','platform_admin']::varchar(50)[],ARRAY['admin:*']::varchar(100)[],'active',$2,TRUE,TRUE,'system')",
    )
    .bind(&email)
    .bind(hash)
    .execute(&mut *tx)
    .await
    .map_err(|_| "Could not create operator; transaction rolled back")?;
    sqlx::query(
        "INSERT INTO gateway.user_roles (user_id,role_name,granted_by) VALUES ('admin','admin','system'),('admin','platform_admin','system')",
    )
    .execute(&mut *tx)
    .await
    .map_err(|_| "Could not assign operator roles; transaction rolled back")?;
    tx.commit().await.map_err(|_| "Operator commit outcome is uncertain; inspect before retrying")?;
    pool.close().await;
    println!("Initial operator created; change its password through the authentication API");
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(message) = run().await {
        eprintln!("{message}");
        std::process::exit(1);
    }
}
