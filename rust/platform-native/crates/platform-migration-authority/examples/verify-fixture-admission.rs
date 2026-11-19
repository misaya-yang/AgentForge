//! Live source-equivalent admission acceptance for an isolated epoch-17 fixture.
use ai_platform_migration_authority::{application_guard::{require_compatible_schema, ApplicationPrincipal}, credentials, AuthorityError, Result};
use sqlx::{Connection, PgConnection};
#[tokio::main]
async fn main() {
    if let Err(error) = run().await { eprintln!("fixture admission verification failed: {error}"); std::process::exit(1); }
}
async fn run() -> Result<()> {
    assert_eq!(std::env::var("AI_PLATFORM_ISOLATED_TEST_FIXTURE").as_deref(),Ok("1"));
    let mut migrator=PgConnection::connect(&credentials::dsn_for_principal("admin")?).await?;
    for principal in [ApplicationPrincipal::KnowledgeApi,ApplicationPrincipal::KnowledgeWorker] {
        require_compatible_schema(&mut migrator,principal).await?;
    }
    match require_compatible_schema(&mut migrator,ApplicationPrincipal::Gateway).await {
        Err(AuthorityError::Refused(message)) if message.contains("outside") => {},
        _ => return Err(AuthorityError::Refused("Gateway must reject epoch 17 outside source window".into())),
    }
    for (role,principal) in [("gateway",ApplicationPrincipal::Gateway),("knowledge_api",ApplicationPrincipal::KnowledgeApi),("knowledge_worker",ApplicationPrincipal::KnowledgeWorker)] {
        let mut conn=PgConnection::connect(&credentials::dsn_for_principal(role)?).await?;
        match require_compatible_schema(&mut conn,principal).await {
            Err(AuthorityError::Database(sqlx::Error::Database(error))) if error.code().as_deref()==Some("42501") => {},
            _ => return Err(AuthorityError::Refused(format!("{role} must preserve source least-privilege permission refusal"))),
        }
        conn.close().await?;
    }
    migrator.close().await?;
    println!("admission fixture passed: Knowledge epoch17 accepted with ledger authority; Gateway epoch17 refused; all3least-privilege roles refused without widened grants");
    Ok(())
}
