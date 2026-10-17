//! Source lint and executed N-1 compatibility rehearsal for one native successor.
#[path = "../migration_lint.rs"]
mod migration_lint;

use ai_platform_migration_authority::{AuthorityError, Result};
use migration_lint::{N1RunOptions, lint_source, rehearse_n1};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Lint,
    RehearseN1,
}

#[derive(Debug)]
struct Options {
    action: Action,
    source_root: PathBuf,
    sequence: i32,
    n1: Option<N1RunOptions>,
}

fn refused(message: &str) -> AuthorityError {
    AuthorityError::Refused(message.into())
}

fn usage() -> &'static str {
    "migration-lint <lint|rehearse-n1> --source-root SOURCE --sequence LATEST [--previous-release ID --previous-application EXECUTABLE --previous-application-sha256 SHA256 --driver EXECUTABLE --driver-sha256 SHA256 --expected-database schema_successor_verify_<8-48 lowercase suffix> --receipt NEW_EXTERNAL_FILE]"
}

fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Option<Options>> {
    let mut args = arguments.into_iter();
    let Some(action) = args.next() else {
        return Err(refused("migration lint action is required"));
    };
    if action == "--help" || action == "-h" {
        return Ok(None);
    }
    let action = match action.as_str() {
        "lint" => Action::Lint,
        "rehearse-n1" => Action::RehearseN1,
        _ => return Err(refused("unsupported migration lint action")),
    };
    let mut source_root = None;
    let mut sequence = None;
    let mut previous_release_id = None;
    let mut previous_application = None;
    let mut previous_application_sha256 = None;
    let mut driver = None;
    let mut driver_sha256 = None;
    let mut expected_database = None;
    let mut receipt_output = None;

    while let Some(flag) = args.next() {
        let slot = match flag.as_str() {
            "--source-root" => &mut source_root,
            "--sequence" => &mut sequence,
            "--previous-release" => &mut previous_release_id,
            "--previous-application" => &mut previous_application,
            "--previous-application-sha256" => &mut previous_application_sha256,
            "--driver" => &mut driver,
            "--driver-sha256" => &mut driver_sha256,
            "--expected-database" => &mut expected_database,
            "--receipt" => &mut receipt_output,
            _ => return Err(refused("unknown migration lint argument")),
        };
        if slot.is_some() {
            return Err(refused("duplicate migration lint argument"));
        }
        let value = args
            .next()
            .ok_or_else(|| refused("migration lint argument value is missing"))?;
        if value.is_empty() || value.starts_with("--") {
            return Err(refused("migration lint argument value is invalid"));
        }
        *slot = Some(value);
    }

    let source_root =
        PathBuf::from(source_root.ok_or_else(|| refused("--source-root is required"))?);
    let sequence = sequence
        .ok_or_else(|| refused("--sequence is required"))?
        .parse::<i32>()
        .map_err(|_| refused("--sequence must be a positive integer"))?;
    if sequence < 1 {
        return Err(refused("--sequence must be a positive integer"));
    }
    let n1_values = [
        previous_release_id.is_some(),
        previous_application.is_some(),
        previous_application_sha256.is_some(),
        driver.is_some(),
        driver_sha256.is_some(),
        expected_database.is_some(),
        receipt_output.is_some(),
    ];
    let n1 = if action == Action::RehearseN1 {
        if n1_values.iter().any(|value| !value) {
            return Err(refused(
                "rehearse-n1 requires the previous release, previous executable and SHA, driver and SHA, isolated database name and receipt path",
            ));
        }
        let expected_database = expected_database.unwrap();
        ai_platform_migration_authority::source_tools::measure_successors::validate_database_name(
            &expected_database,
        )?;
        Some(N1RunOptions {
            previous_release_id: previous_release_id.unwrap(),
            previous_application: PathBuf::from(previous_application.unwrap()),
            previous_application_sha256: previous_application_sha256.unwrap(),
            driver: PathBuf::from(driver.unwrap()),
            driver_sha256: driver_sha256.unwrap(),
            expected_database,
            receipt_output: PathBuf::from(receipt_output.unwrap()),
        })
    } else {
        if n1_values.iter().any(|value| *value) {
            return Err(refused(
                "N-1 executable and receipt options are only valid for rehearse-n1",
            ));
        }
        None
    };
    Ok(Some(Options {
        action,
        source_root,
        sequence,
        n1,
    }))
}

fn execute(options: Options) -> Result<serde_json::Value> {
    match options.action {
        Action::Lint => serde_json::to_value(lint_source(&options.source_root, options.sequence)?)
            .map_err(|_| refused("migration lint report could not be serialized")),
        Action::RehearseN1 => serde_json::to_value(rehearse_n1(
            &options.source_root,
            options.sequence,
            options
                .n1
                .ok_or_else(|| refused("N-1 rehearsal options are missing"))?,
        )?)
        .map_err(|_| refused("N-1 rehearsal receipt could not be serialized")),
    }
}

fn main() {
    let result = match parse(std::env::args().skip(1)) {
        Ok(None) => {
            println!("{}", usage());
            return;
        }
        Ok(Some(options)) => execute(options),
        Err(error) => Err(error),
    };
    match result {
        Ok(value) => println!("{value}"),
        Err(error) => {
            eprintln!("migration-lint: {error}; usage: {}", usage());
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    #[test]
    fn lint_action_does_not_accept_n1_claims_or_arbitrary_sql() {
        assert!(
            parse(args(&[
                "lint",
                "--source-root",
                "/source",
                "--sequence",
                "25"
            ]))
            .unwrap()
            .is_some()
        );
        assert!(
            parse(args(&[
                "lint",
                "--source-root",
                "/source",
                "--sequence",
                "25",
                "--previous-release",
                "fake-pass"
            ]))
            .is_err()
        );
        assert!(
            parse(args(&[
                "lint",
                "--source-root",
                "/source",
                "--sequence",
                "25",
                "--sql",
                "/arbitrary.sql"
            ]))
            .is_err()
        );
    }

    #[test]
    fn n1_action_requires_an_executable_contract_and_isolated_database_name() {
        let missing_driver = args(&[
            "rehearse-n1",
            "--source-root",
            "/source",
            "--sequence",
            "25",
            "--previous-release",
            "gateway-1.2.3",
            "--previous-application",
            "/n1/gateway",
            "--previous-application-sha256",
            &"a".repeat(64),
            "--expected-database",
            "schema_successor_verify_20261017",
            "--receipt",
            "/external/receipt.json",
        ]);
        assert!(parse(missing_driver).is_err());

        let bad_database = args(&[
            "rehearse-n1",
            "--source-root",
            "/source",
            "--sequence",
            "25",
            "--previous-release",
            "gateway-1.2.3",
            "--previous-application",
            "/n1/gateway",
            "--previous-application-sha256",
            &"a".repeat(64),
            "--driver",
            "/n1/driver",
            "--driver-sha256",
            &"b".repeat(64),
            "--expected-database",
            "postgres",
            "--receipt",
            "/external/receipt.json",
        ]);
        assert!(parse(bad_database).is_err());
    }
}
