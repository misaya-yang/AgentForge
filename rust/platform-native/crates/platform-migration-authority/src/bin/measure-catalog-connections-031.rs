use ai_platform_migration_authority::{AuthorityError, source_tools::catalog_connections_031};
use std::path::PathBuf;

#[tokio::main]
async fn main() {
    if run().await.is_err() {
        eprintln!(
            "031 measurement refused or failed; verify source, isolated database, migrator role and authority environment (credential values withheld)"
        );
        std::process::exit(1);
    }
}
async fn run() -> ai_platform_migration_authority::Result<()> {
    let mut root = PathBuf::from(".");
    let mut database = None;
    let mut approved = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => {
                root = PathBuf::from(
                    args.next()
                        .ok_or_else(|| AuthorityError::Refused("missing root".into()))?,
                )
            }
            "--expected-database" => database = args.next(),
            "--run-approved-isolated-measurements" => approved = true,
            _ => return Err(AuthorityError::Refused("unknown measurement option".into())),
        }
    }
    let database =
        database.ok_or_else(|| AuthorityError::Refused("expected database required".into()))?;
    catalog_connections_031::measure(&root.canonicalize()?, &database, approved).await?;
    println!("031 measured; exact predecessor verified and post-apply replay was a no-op");
    Ok(())
}
