//! Explicit opt-in admin prerequisite for isolated PostgreSQL acceptance fixtures.
//! Uses the same helper as the historical acceptance fixture, without adding a production CLI.
use ai_platform_migration_authority::{bootstrap, credentials, runner::AuthorityPaths, Result};
use sqlx::{Connection, PgConnection};
#[tokio::main]
async fn main() {
    if run().await.is_err() {
        eprintln!("fixture extension provisioning failed");
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    assert_eq!(std::env::var("AI_PLATFORM_ISOLATED_TEST_FIXTURE").as_deref(), Ok("1"), "isolated fixture opt-in required");
    let mut conn = PgConnection::connect(&credentials::dsn_for_principal("admin")?).await?;
    let paths = AuthorityPaths { database_dir: std::env::var_os("AI_GATEWAY_DATABASE_DIR").expect("fixture database directory required").into() };
    bootstrap::provision_extensions_admin(&mut conn, &paths).await
}
