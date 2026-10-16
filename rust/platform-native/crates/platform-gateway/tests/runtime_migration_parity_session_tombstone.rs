//! Source session-delete tombstone regression against actual PostgreSQL storage.
//! Only an explicitly named disposable loopback DB; ordinary tests do no I/O.
use ai_platform_gateway::{session_manager::SessionStorage, session_storage::PgSessionStorage};
use futures_util::FutureExt;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
#[tokio::test]
#[ignore = "requires empty loopback database runtime_parity_* via AI_PLATFORM_RUNTIME_PERSISTENCE_TEST_DATABASE_URL"]
async fn unreferenced_delete_and_restricted_tombstone_preserve_runtime_audit_chain() -> Result<()> {
    let dsn = std::env::var("AI_PLATFORM_RUNTIME_PERSISTENCE_TEST_DATABASE_URL")
        .map_err(|_| "explicit disposable PostgreSQL test URL required")?;
    let parsed = url::Url::parse(&dsn).map_err(|_| "invalid PostgreSQL fixture URL")?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql")
        || !matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
        || !parsed
            .path()
            .trim_start_matches('/')
            .starts_with("runtime_parity_")
    {
        return Err("fixture requires loopback database named runtime_parity_*".into());
    }
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&dsn)
        .await?;
    let control = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&dsn)
        .await?;
    let storage = PgSessionStorage::new(pool.clone(), control.clone())?;
    // Never replace a schema or table. This guard and CREATE SCHEMA fail closed
    // if another test/operator already owns the requested fixture database.
    let occupied:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_schema NOT IN ('pg_catalog','information_schema')) OR EXISTS(SELECT 1 FROM pg_namespace WHERE nspname='assistant')").fetch_one(&pool).await?;
    if occupied {
        return Err(
            "disposable database must contain no application tables or assistant schema".into(),
        );
    }
    sqlx::query("CREATE SCHEMA assistant")
        .execute(&pool)
        .await?;
    let result=std::panic::AssertUnwindSafe(async {
        sqlx::query("CREATE TABLE assistant.sessions(session_id text PRIMARY KEY,status text NOT NULL DEFAULT 'active',created_at timestamptz NOT NULL DEFAULT NOW(),updated_at timestamptz NOT NULL DEFAULT NOW(),expires_at timestamptz)").execute(&pool).await?;
        sqlx::query("CREATE TABLE assistant.audit_fixture(id text PRIMARY KEY,session_id text NOT NULL REFERENCES assistant.sessions(session_id) ON DELETE RESTRICT)").execute(&pool).await?;
        sqlx::query("INSERT INTO assistant.sessions(session_id) VALUES('unreferenced'),('retained')").execute(&pool).await?;
        sqlx::query("INSERT INTO assistant.audit_fixture VALUES('audit','retained')").execute(&pool).await?;
        assert!(storage.delete("unreferenced").await?);
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM assistant.sessions WHERE session_id='unreferenced'").fetch_one(&pool).await?;assert_eq!(count,0);
        assert!(storage.delete("retained").await?);
        let status:String=sqlx::query_scalar("SELECT status FROM assistant.sessions WHERE session_id='retained'").fetch_one(&pool).await?;assert_eq!(status,"deleted");
        let audit:i64=sqlx::query_scalar("SELECT count(*) FROM assistant.audit_fixture WHERE session_id='retained'").fetch_one(&pool).await?;assert_eq!(audit,1);
        assert_eq!(storage.get("retained").await?,None);
        assert_eq!(storage.get("unreferenced").await?,None);
        Ok::<(),Box<dyn std::error::Error+Send+Sync>>(())
    }).catch_unwind().await;
    storage.drain_cancelled().await?;
    // Only the schema created by this fixture is removed, after owned operation drain.
    let cleanup = sqlx::query("DROP SCHEMA assistant CASCADE")
        .execute(&pool)
        .await;
    pool.close().await;
    control.close().await;
    cleanup?;
    match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
