//! Explicit one-shot admin bootstrap. This binary is never run by service startup.
use sqlx::{Connection, PgConnection};
#[path = "admin_create/fresh.rs"]
mod fresh;
fn required(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{name} must be set"))
}
async fn run() -> Result<(), String> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "admin-create: legacy mode requires DATABASE_URL, ADMIN_EMAIL and ADMIN_PASSWORD and refuses duplicate rows. --fresh-development reads a bounded JSON email/password object from stdin and canonical DATA_GATEWAY file/mTLS configuration; initializes only gateway_dev_fresh. Fresh mode creates the tenant and current admin role binding once, retains first-login password change, and only verifies an already matching active administrator. Neither mode resets or upserts accounts."
        );
        return Ok(());
    }
    if arguments == ["--fresh-development"] {
        return fresh::run().await.map_err(str::to_owned);
    }
    if !arguments.is_empty() {
        return Err("unknown admin-create arguments".into());
    }
    let dsn = required("DATABASE_URL")?;
    let email = required("ADMIN_EMAIL")?;
    let password = required("ADMIN_PASSWORD")?;
    let mut conn = PgConnection::connect(&dsn)
        .await
        .map_err(|_| "database connection failed".to_owned())?;
    let hash = tokio::task::spawn_blocking(move || ai_platform_auth::hash_password(&password))
        .await
        .map_err(|_| "password hashing failed".to_owned())?
        .map_err(|_| "password hashing failed".to_owned())?;
    let outcome=sqlx::query("INSERT INTO users (user_id, email, password_hash, status, roles) VALUES ($1, $2, $3, $4, $5)").bind("admin").bind(email).bind(hash).bind("active").bind(vec!["admin"]).execute(&mut conn).await;
    conn.close()
        .await
        .map_err(|_| "database disconnect failed".to_owned())?;
    outcome.map_err(|_| "admin insertion failed".to_owned())?;
    println!("Admin created");
    Ok(())
}
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
