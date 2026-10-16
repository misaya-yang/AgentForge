use ai_platform_migration_authority::{
    AuthorityError, DEFAULT_BASELINE_ID, Result,
    source_tools::freeze::{FreezeOptions, freeze},
};
use std::path::PathBuf;
async fn run() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let mut options = FreezeOptions {
        baseline_dir: root.join("database/baselines").join(DEFAULT_BASELINE_ID),
        root,
        source_database: String::new(),
        verify_database: String::new(),
        pg_dump: "pg_dump".into(),
        write: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--write" {
            options.write = true;
            continue;
        }
        if arg == "--help" || arg == "-h" {
            println!(
                "freeze-baseline --source-database arc03_NAME --verify-database arc03_NAME [--baseline-dir PATH] [--pg-dump BINARY] [--write]"
            );
            return Ok(());
        }
        let value = args
            .next()
            .ok_or_else(|| AuthorityError::Refused("missing option value".into()))?;
        match arg.as_str() {
            "--baseline-dir" => options.baseline_dir = value.into(),
            "--source-database" => options.source_database = value,
            "--verify-database" => options.verify_database = value,
            "--pg-dump" => options.pg_dump = value,
            _ => return Err(AuthorityError::Refused("unknown option".into())),
        }
    }
    freeze(options).await
}
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("BLOCKED: {error}");
        std::process::exit(2);
    }
}
