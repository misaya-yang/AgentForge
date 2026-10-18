//! Offline operator artifact review only. No credentials or SQL execution mode.
use ai_platform_migration_authority::{
    AuthorityError, Result, source_tools::service_successors::operator_stage,
};
use std::{collections::BTreeMap, path::PathBuf};
fn parse(args: impl IntoIterator<Item = String>) -> Result<BTreeMap<String, PathBuf>> {
    let mut args = args.into_iter();
    let mut values = BTreeMap::new();
    while let Some(key) = args.next() {
        if !["--source-root", "--operation-plan", "--evidence-files"].contains(&key.as_str())
            || values.contains_key(&key)
        {
            return Err(AuthorityError::Refused(
                "unknown/duplicate partition review option; no execute mode".into(),
            ));
        }
        let value = args
            .next()
            .filter(|s| !s.is_empty() && !s.starts_with("--"))
            .ok_or_else(|| {
                AuthorityError::Refused("partition review option requires path".into())
            })?;
        values.insert(key, PathBuf::from(value));
    }
    if values.len() != 3 {
        return Err(AuthorityError::Refused(
            "source-root, operation-plan and evidence-files required".into(),
        ));
    }
    Ok(values)
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h") {
        println!(
            "partition-stage-review --source-root SOURCE --operation-plan NATIVE_PLAN_JSON --evidence-files SEVEN_ARTIFACT_FILES_JSON (offline; no execute/approval/acceptance)"
        );
        return;
    }
    let result = parse(args).and_then(|v| {
        operator_stage::review_files(
            &v["--source-root"],
            &v["--operation-plan"],
            &v["--evidence-files"],
        )
    });
    match result {
        Ok(v) => println!("{v}"),
        Err(e) => {
            eprintln!("partition stage review refused: {e}");
            std::process::exit(1);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_execute_or_arbitrary_sql_interface() {
        for a in [
            vec!["--execute"],
            vec!["--source-root", "/s", "--operation-plan", "/p"],
            vec![
                "--source-root",
                "/s",
                "--operation-plan",
                "/p",
                "--evidence-files",
                "/e",
                "--sql",
                "DROP",
            ],
        ] {
            assert!(parse(a.into_iter().map(str::to_owned)).is_err());
        }
        assert!(
            parse(
                [
                    "--source-root",
                    "/s",
                    "--operation-plan",
                    "/p",
                    "--evidence-files",
                    "/e"
                ]
                .into_iter()
                .map(str::to_owned)
            )
            .is_ok()
        );
    }
}
