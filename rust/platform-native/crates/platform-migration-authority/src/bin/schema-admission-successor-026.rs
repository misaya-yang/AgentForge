//! Fixed turn-persistence source/measurement tool, deliberately separate from
//! release admission/main. No SQL, reference hash or role override arguments.
use ai_platform_migration_authority::{
    AuthorityError, Result, source_tools::next_successor_026 as successor,
};
use std::path::PathBuf;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Validate,
    Prepare,
    VerifyPreview,
    Measure,
    VerifyMeasured,
    Bundle,
    VerifyActivated,
}
#[derive(Debug)]
struct Options {
    action: Action,
    source: PathBuf,
    preview: Option<PathBuf>,
    output: Option<PathBuf>,
    database: Option<String>,
    approved: bool,
}
fn refused(message: &str) -> AuthorityError {
    AuthorityError::Refused(message.into())
}
fn usage() -> &'static str {
    "schema-admission-successor-026 <validate|prepare|verify-preview|measure|verify-measured|bundle|verify-activated> --source-root SOURCE [--preview-root EXTERNAL_PREVIEW] [--output NEW_EXTERNAL_DIRECTORY] [--expected-database schema_successor_verify_<8-48 lowercase suffix> --run-approved-isolated-measurements]"
}
fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Option<Options>> {
    let mut args = arguments.into_iter();
    let Some(first) = args.next() else {
        return Err(refused("026 action is required"));
    };
    let action = match first.as_str() {
        "--help" | "-h" => return Ok(None),
        "validate" => Action::Validate,
        "prepare" => Action::Prepare,
        "verify-preview" => Action::VerifyPreview,
        "measure" => Action::Measure,
        "verify-measured" => Action::VerifyMeasured,
        "bundle" => Action::Bundle,
        "verify-activated" => Action::VerifyActivated,
        _ => return Err(refused("026 unsupported action")),
    };
    let mut source = None;
    let mut preview = None;
    let mut output = None;
    let mut database = None;
    let mut approved = false;
    while let Some(arg) = args.next() {
        if arg == "--run-approved-isolated-measurements" {
            if approved {
                return Err(refused("026 duplicate approval flag"));
            }
            approved = true;
            continue;
        }
        let slot = match arg.as_str() {
            "--source-root" => &mut source,
            "--preview-root" => &mut preview,
            "--output" => &mut output,
            "--expected-database" => &mut database,
            _ => {
                return Err(refused(
                    "026 unknown argument; arbitrary SQL and hash overrides are forbidden",
                ));
            }
        };
        if slot.is_some() {
            return Err(refused("026 duplicate argument"));
        }
        let value = args
            .next()
            .ok_or_else(|| refused("026 missing argument value"))?;
        if value.is_empty() || value.starts_with("--") {
            return Err(refused("026 invalid argument value"));
        }
        *slot = Some(value);
    }
    let source = PathBuf::from(source.ok_or_else(|| refused("026 --source-root is required"))?);
    let needs_preview = matches!(
        action,
        Action::VerifyPreview | Action::Measure | Action::VerifyMeasured | Action::Bundle
    );
    let needs_output = matches!(action, Action::Prepare | Action::Bundle);
    if preview.is_some() != needs_preview || output.is_some() != needs_output {
        return Err(refused(
            "026 preview/output options do not match the selected action",
        ));
    }
    if action == Action::Measure {
        if !approved || database.is_none() {
            return Err(refused(
                "026 measurement requires a named isolated database and explicit approval",
            ));
        }
        ai_platform_migration_authority::source_tools::measure_successors::validate_database_name(
            database
                .as_deref()
                .ok_or_else(|| refused("026 isolated database is missing"))?,
        )?;
    } else if approved || database.is_some() {
        return Err(refused("026 database options are only valid for measure"));
    }
    Ok(Some(Options {
        action,
        source,
        preview: preview.map(PathBuf::from),
        output: output.map(PathBuf::from),
        database,
        approved,
    }))
}
async fn execute(options: Options) -> Result<serde_json::Value> {
    let preview = || {
        options
            .preview
            .as_deref()
            .ok_or_else(|| refused("026 preview is required"))
    };
    let output = || {
        options
            .output
            .as_deref()
            .ok_or_else(|| refused("026 output is required"))
    };
    match options.action {
        Action::Validate => {
            let candidate = successor::validate_source(&options.source)?;
            Ok(
                serde_json::json!({"status":"reviewed_026_source_verified","source_plan_sha256":candidate.plan_sha256,"predecessor_epoch":25,"candidate_epoch":26,"database_actions":false}),
            )
        }
        Action::Prepare => successor::materialize(&options.source, output()?),
        Action::VerifyPreview | Action::VerifyMeasured => {
            let measured = options.action == Action::VerifyMeasured;
            let candidate = successor::verify_preview(&options.source, preview()?, measured)?;
            Ok(
                serde_json::json!({"status":if measured{"measured_026_preview_verified"}else{"inactive_026_preview_verified"},"source_plan_sha256":candidate.plan_sha256,"predecessor_epoch":25,"candidate_epoch":26,"database_actions":false}),
            )
        }
        Action::Bundle => successor::activation_bundle(&options.source, preview()?, output()?),
        Action::VerifyActivated => successor::verify_activated_source(&options.source),
        Action::Measure => {
            successor::measure(successor::MeasureOptions {
                source_root: options.source,
                preview_root: options
                    .preview
                    .ok_or_else(|| refused("026 preview is required"))?,
                expected_database: options
                    .database
                    .ok_or_else(|| refused("026 database is required"))?,
                run_approved: options.approved,
            })
            .await?;
            Ok(
                serde_json::json!({"status":"measured_026_external_preview","predecessor_epoch":25,"candidate_epoch":26,"post_migration_noop":true,"source_activation":false}),
            )
        }
    }
}
#[tokio::main]
async fn main() {
    let result = match parse(std::env::args().skip(1)) {
        Ok(None) => {
            println!("{}", usage());
            return;
        }
        Ok(Some(options)) => execute(options).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(value) => println!("{value}"),
        Err(error) => {
            if let AuthorityError::Database(driver) = &error {
                if let Some(database) = driver.as_database_error() {
                    if let Some(code) = database.code().filter(|code| {
                        code.len() == 5 && code.bytes().all(|byte| byte.is_ascii_alphanumeric())
                    }) {
                        eprintln!("isolated026 database SQLSTATE={code}");
                    }
                    if let Some(postgres) =
                        database.try_downcast_ref::<sqlx::postgres::PgDatabaseError>()
                    {
                        if let Some(sqlx::postgres::PgErrorPosition::Original(position)) =
                            postgres.position()
                        {
                            eprintln!("isolated026 source statement position={position}");
                        }
                    }
                }
            }
            eprintln!(
                "schema-admission-successor-026: {error}; usage: {}",
                usage()
            );
            std::process::exit(1);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).into()).collect()
    }
    #[test]
    fn fixed_actions_and_approved_measurement_parse() {
        assert_eq!(
            parse(args(&["validate", "--source-root", "/source"]))
                .unwrap()
                .unwrap()
                .action,
            Action::Validate
        );
        let options = parse(args(&[
            "measure",
            "--source-root",
            "/source",
            "--preview-root",
            "/preview",
            "--expected-database",
            "schema_successor_verify_nativecontracts026",
            "--run-approved-isolated-measurements",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(options.action, Action::Measure);
        assert!(options.approved);
        assert_eq!(
            parse(args(&[
                "bundle",
                "--source-root",
                "/source",
                "--preview-root",
                "/preview",
                "--output",
                "/bundle"
            ]))
            .unwrap()
            .unwrap()
            .action,
            Action::Bundle
        );
    }
    #[test]
    fn rejects_sql_hash_role_overrides_and_unsafe_option_combinations() {
        for values in [
            vec![
                "measure",
                "--source-root",
                "/source",
                "--preview-root",
                "/preview",
            ],
            vec![
                "measure",
                "--source-root",
                "/source",
                "--preview-root",
                "/preview",
                "--expected-database",
                "postgres",
                "--run-approved-isolated-measurements",
            ],
            vec![
                "prepare",
                "--source-root",
                "/source",
                "--output",
                "/new",
                "--output",
                "/other",
            ],
            vec![
                "validate",
                "--source-root",
                "/source",
                "--sql",
                "/arbitrary.sql",
            ],
            vec![
                "validate",
                "--source-root",
                "/source",
                "--fingerprint",
                "fake",
            ],
            vec!["validate", "--source-root", "/source", "--role", "owner"],
            vec![
                "validate",
                "--source-root",
                "/source",
                "--run-approved-isolated-measurements",
            ],
            vec!["bundle", "--source-root", "/source", "--output", "/bundle"],
            vec![
                "verify-preview",
                "--source-root",
                "/source",
                "--preview-root",
                "--output",
            ],
        ] {
            assert!(parse(args(&values)).is_err(), "{values:?}");
        }
    }
}
