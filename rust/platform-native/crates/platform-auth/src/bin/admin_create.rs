//! Explicit one-shot admin bootstrap. This binary is never run by service startup.
use sqlx::{Connection, PgConnection};
fn required(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{name} must be set"))
}
async fn run() -> Result<(), String> {
    if std::env::args().skip(1).any(|a| a == "--help" || a == "-h") {
        println!(
            "admin-create: requires DATABASE_URL, ADMIN_EMAIL and ADMIN_PASSWORD; inserts user_id=admin once. Duplicate rows fail; no reset or upsert."
        );
        return Ok(());
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
