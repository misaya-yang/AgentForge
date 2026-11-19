use ai_platform_migration_authority::{
    AuthorityError, DEFAULT_ROLE_PREFIX,
    source_tools::probe::{ProbeError, ProbeOptions, run},
};
use std::path::PathBuf;
async fn execute() -> std::result::Result<serde_json::Value, ProbeError> {
    let mut o = ProbeOptions {
        root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.."),
        source_root: None,
        preimage_root: None,
        run: false,
        shadow: false,
        database: String::new(),
        prefix: DEFAULT_ROLE_PREFIX.into(),
        dataset: String::new(),
        other_dataset: String::new(),
        rule: String::new(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--run-approved-isolated-probes" => {
                o.run = true;
                continue;
            }
            "--run-approved-temporary-shadow-probes" => {
                o.shadow = true;
                continue;
            }
            "--help" | "-h" => {
                return Ok(
                    serde_json::json!({"usage":"probe-successor-roles [--root CANDIDATE_DIR] [--source-root SOURCE_DIR] [--preimage-root PRESERVED_CHECKOUT] [--run-approved-isolated-probes --database NAME --role-prefix PREFIX --dataset ID --other-dataset ID --process-rule ID] [--run-approved-temporary-shadow-probes]"}),
                );
            }
            _ => {}
        }
        let value = args
            .next()
            .ok_or_else(|| AuthorityError::Refused("missing option value".into()))?;
        match a.as_str() {
            "--root" => o.root = value.into(),
            "--source-root" => o.source_root = Some(value.into()),
            "--preimage-root" => o.preimage_root = Some(value.into()),
            "--database" => o.database = value,
            "--role-prefix" => o.prefix = value,
            "--dataset" => o.dataset = value,
            "--other-dataset" => o.other_dataset = value,
            "--process-rule" => o.rule = value,
            _ => return Err(AuthorityError::Refused("unknown option".into()).into()),
        }
    }
    run(o).await
}
#[tokio::main]
async fn main() {
    match execute().await {
        Ok(value) => println!("{value}"),
        Err(error) => {
            println!(
                "{}",
                serde_json::json!({"status":"failed","error_class":error.error_class(),"phase":error.phase(),"role":error.role(),"sqlstate":error.sqlstate()})
            );
            std::process::exit(1);
        }
    }
}
