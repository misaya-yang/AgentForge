//! Closed030..035 source-owned service chain. Preparation never activates source/DB.
use ai_platform_migration_authority::{
    AuthorityError, Result, source_tools::service_successors as pack,
};
use std::{collections::BTreeMap, path::PathBuf};
fn denied(s: &str) -> AuthorityError {
    AuthorityError::Refused(s.into())
}
#[derive(Debug)]
struct Options {
    action: String,
    source: PathBuf,
    preview: Option<PathBuf>,
    previous: Option<PathBuf>,
    output: Option<PathBuf>,
    sequence: i32,
    database: Option<String>,
    approved: bool,
    measured: bool,
}
fn parse(args: impl IntoIterator<Item = String>) -> Result<Options> {
    let mut args = args.into_iter();
    let action = args
        .next()
        .ok_or_else(|| denied("service successor action required"))?;
    if ![
        "validate",
        "prepare",
        "verify-preview",
        "measure",
        "bundle",
        "verify-activated",
    ]
    .contains(&action.as_str())
    {
        return Err(denied("unknown service successor action"));
    }
    let mut values = BTreeMap::new();
    let mut approved = false;
    let mut measured = false;
    while let Some(key) = args.next() {
        if key == "--run-approved-isolated-measurements" {
            if approved {
                return Err(denied("duplicate approval"));
            }
            approved = true;
            continue;
        }
        if key == "--measured" {
            if measured {
                return Err(denied("duplicate measured flag"));
            }
            measured = true;
            continue;
        }
        if ![
            "--source-root",
            "--preview-root",
            "--previous-preview",
            "--output",
            "--sequence",
            "--expected-database",
        ]
        .contains(&key.as_str())
            || values.contains_key(&key)
        {
            return Err(denied(
                "unknown/duplicate option; arbitrary SQL/hash overrides refused",
            ));
        }
        let value = args
            .next()
            .filter(|v| !v.is_empty() && !v.starts_with("--"))
            .ok_or_else(|| denied("missing option value"))?;
        values.insert(key, value);
    }
    let source = PathBuf::from(
        values
            .remove("--source-root")
            .ok_or_else(|| denied("source-root required"))?,
    );
    let sequence = values
        .remove("--sequence")
        .map(|s| {
            s.parse::<i32>()
                .map_err(|_| denied("invalid fixed sequence"))
        })
        .transpose()?
        .unwrap_or(0);
    if action != "verify-activated" && !(30..=35).contains(&sequence) {
        return Err(denied("only source-owned030..035 accepted"));
    }
    if action == "verify-activated" && sequence != 0 {
        return Err(denied("activated source selects its exact catalog prefix"));
    }
    let preview = values.remove("--preview-root").map(PathBuf::from);
    let previous = values.remove("--previous-preview").map(PathBuf::from);
    let output = values.remove("--output").map(PathBuf::from);
    let database = values.remove("--expected-database");
    if preview.is_some() != matches!(action.as_str(), "verify-preview" | "measure" | "bundle")
        || output.is_some() != matches!(action.as_str(), "prepare" | "bundle")
        || previous.is_some() != (action == "prepare" && sequence > 30)
        || measured && action != "verify-preview"
        || approved != (action == "measure")
        || database.is_some() != (action == "measure")
    {
        return Err(denied(
            "action/arguments do not match; no implicit measurement or activation",
        ));
    }
    if let Some(db) = &database {
        ai_platform_migration_authority::source_tools::measure_successors::validate_database_name(
            db,
        )?;
    }
    Ok(Options {
        action,
        source,
        preview,
        previous,
        output,
        sequence,
        database,
        approved,
        measured,
    })
}
async fn run(o: Options) -> Result<serde_json::Value> {
    match o.action.as_str() {
        "validate" => pack::validate_source(&o.source, o.sequence),
        "prepare" => pack::prepare(
            &o.source,
            o.previous.as_deref(),
            o.output
                .as_deref()
                .ok_or_else(|| denied("output missing"))?,
            o.sequence,
        ),
        "verify-preview" => pack::verify_preview(
            &o.source,
            o.preview
                .as_deref()
                .ok_or_else(|| denied("preview missing"))?,
            o.sequence,
            o.measured,
        ),
        "measure" => {
            pack::measure(pack::MeasureOptions {
                source_root: o.source,
                preview_root: o.preview.ok_or_else(|| denied("preview missing"))?,
                sequence: o.sequence,
                expected_database: o.database.ok_or_else(|| denied("database missing"))?,
                run_approved: o.approved,
            })
            .await?;
            Ok(serde_json::json!({"status":"measured_service_successor","source_activation":false}))
        }
        "bundle" => pack::activation_bundle(
            &o.source,
            o.preview
                .as_deref()
                .ok_or_else(|| denied("preview missing"))?,
            o.output
                .as_deref()
                .ok_or_else(|| denied("output missing"))?,
            o.sequence,
        ),
        "verify-activated" => pack::verify_activated_source(&o.source),
        _ => Err(denied("unsupported action")),
    }
}
#[tokio::main]
async fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h") {
        println!(
            "schema-admission-service-successors <validate|prepare|verify-preview|measure|bundle|verify-activated> --source-root SOURCE [--sequence 30|31|32|33|34|35] [--previous-preview MEASURED_PREVIOUS] [--preview-root PREVIEW] [--output NEW_EXTERNAL] [--measured] [--expected-database schema_successor_verify_SUFFIX --run-approved-isolated-measurements]"
        );
        return;
    }
    let result = match parse(args) {
        Ok(o) => run(o).await,
        Err(e) => Err(e),
    };
    match result {
        Ok(value) => println!("{value}"),
        Err(error) => {
            eprintln!("service-successors: {error}");
            std::process::exit(1);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).into()).collect()
    }
    #[test]
    fn only_closed_fixed_actions_are_accepted() {
        assert!(
            parse(args(&[
                "prepare",
                "--source-root",
                "/source",
                "--sequence",
                "30",
                "--output",
                "/external"
            ]))
            .is_ok()
        );
        assert!(
            parse(args(&[
                "prepare",
                "--source-root",
                "/source",
                "--sequence",
                "31",
                "--previous-preview",
                "/measured30",
                "--output",
                "/external"
            ]))
            .is_ok()
        );
        for a in [
            vec!["activate"],
            vec!["validate", "--source-root", "/source", "--sequence", "36"],
            vec![
                "validate",
                "--source-root",
                "/source",
                "--sequence",
                "30",
                "--sha256",
                "fake",
            ],
            vec![
                "prepare",
                "--source-root",
                "/source",
                "--sequence",
                "31",
                "--output",
                "/external",
            ],
            vec![
                "measure",
                "--source-root",
                "/source",
                "--sequence",
                "30",
                "--preview-root",
                "/preview",
                "--expected-database",
                "postgres",
                "--run-approved-isolated-measurements",
            ],
        ] {
            assert!(parse(args(&a)).is_err(), "{a:?}");
        }
    }
    #[test]
    fn measurement_requires_explicit_approved_database_and_no_ignored_flags() {
        let base = [
            "measure",
            "--source-root",
            "/source",
            "--sequence",
            "30",
            "--preview-root",
            "/preview",
            "--expected-database",
            "schema_successor_verify_service030",
        ];
        assert!(parse(args(&base)).is_err());
        let mut approved = args(&base);
        approved.push("--run-approved-isolated-measurements".into());
        assert!(parse(approved).is_ok());
        assert!(
            parse(args(&[
                "validate",
                "--source-root",
                "/source",
                "--sequence",
                "30",
                "--measured"
            ]))
            .is_err()
        );
    }
}
