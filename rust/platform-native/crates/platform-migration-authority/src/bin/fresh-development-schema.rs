//! One fresh install + actual verification, independently of upgrade admission.
use ai_platform_migration_authority::{AuthorityError, Result, fresh_schema};
use std::path::PathBuf;
async fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let command=args.first().map(String::as_str).ok_or_else(||AuthorityError::Refused("usage: fresh-development-schema {init,verify,contract,inventory} --source-root PATH --expected-database gateway_dev_fresh".into()))?;
    if command == "contract" {
        if args.len() != 1 {
            return Err(AuthorityError::Refused(
                "contract takes no arguments".into(),
            ));
        }
        println!("{}", fresh_schema::admission_contract());
        return Ok(());
    }
    if !["init", "verify", "inventory"].contains(&command) {
        return Err(AuthorityError::Refused(
            "unknown fresh-development-schema command".into(),
        ));
    }
    let mut root = None;
    let mut expected = None;
    let mut at = 1;
    while at < args.len() {
        let value = args
            .get(at + 1)
            .ok_or_else(|| AuthorityError::Refused("fresh option requires a value".into()))?;
        match args[at].as_str() {
            "--source-root" if root.is_none() => root = Some(PathBuf::from(value)),
            "--expected-database" if expected.is_none() => expected = Some(value.clone()),
            _ => {
                return Err(AuthorityError::Refused(
                    "unknown or repeated fresh option".into(),
                ));
            }
        }
        at += 2;
    }
    let value = if command == "inventory" {
        if expected.is_some() {
            return Err(AuthorityError::Refused(
                "inventory does not connect to a database".into(),
            ));
        }
        fresh_schema::source_inventory(
            &root.ok_or_else(|| {
                AuthorityError::Refused("inventory requires --source-root".into())
            })?,
        )?
    } else {
        if root.is_some() {
            return Err(AuthorityError::Refused("fresh init/verify use the complete compiled definitions; source-root is inventory-only".into()));
        }
        let expected = expected.ok_or_else(|| {
            AuthorityError::Refused("--expected-database gateway_dev_fresh is required".into())
        })?;
        if command == "init" {
            fresh_schema::initialize_with_local_operator(
                &expected,
                &fresh_schema::LocalOperator::from_env()?,
            )
            .await?
        } else {
            fresh_schema::verify(&expected).await?
        }
    };
    println!("{value}");
    Ok(())
}
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
