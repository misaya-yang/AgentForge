use ai_platform_migration_authority::{AuthorityError, Result, source_tools::metrics};
use sqlx::{Connection, PgConnection};
async fn run() -> Result<()> {
    let mut dsn = std::env::var("GATEWAY_DATABASE__DSN").unwrap_or_default();
    let mut apply = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--apply" => apply = true,
            "--dsn" => {
                dsn = args
                    .next()
                    .ok_or_else(|| AuthorityError::Refused("--dsn requires a value".into()))?
            }
            "--help" | "-h" => {
                println!(
                    "repair-usage-metrics [--dsn DSN] [--apply]\nDefault: dry-run; DSN from GATEWAY_DATABASE__DSN. Apply removes exact duplicates and rebuilds both aggregates in one transaction."
                );
                return Ok(());
            }
            _ => return Err(AuthorityError::Refused("unknown repair option".into())),
        }
    }
    let mut conn = PgConnection::connect(&dsn).await?;
    let result: Result<()> = async {
        print!(
            "{}",
            metrics::format_snapshot("Before", &metrics::snapshot(&mut conn).await?)
        );
        if !apply {
            println!("\nDry-run only. Re-run with --apply to execute repair.");
            return Ok(());
        }
        let removed = metrics::repair(&mut conn).await?;
        let after = metrics::snapshot(&mut conn).await?;
        println!("\nRemoved duplicate usage_records rows: {removed}");
        print!("{}", metrics::format_snapshot("After", &after));
        Ok(())
    }
    .await;
    let closed = conn.close().await.map_err(AuthorityError::from);
    result.and(closed)
}
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
