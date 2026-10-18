//! Offline native-authority plan exporter. No DSN, credentials or DB execution.
use ai_platform_tools::data_security::Result;
use ai_platform_tools::data_security::append_partitions::{
    self as append, Operation, Snapshot, catalog,
};
use serde_json::json;
use std::{collections::BTreeMap, path::Path};
fn run() -> Result<serde_json::Value> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).ok_or(
        "usage: platform-append-partitions catalog|snapshot|maintenance|plan|verify-archive [options]",
    )?;
    let mut options = BTreeMap::new();
    let mut i = 1;
    while i < args.len() {
        let key = &args[i];
        if ![
            "--catalog",
            "--authority-snapshot",
            "--snapshot",
            "--operation",
            "--now",
            "--manifest",
            "--receipt",
            "--snapshots",
            "--daily-ahead",
            "--monthly-ahead",
        ]
        .contains(&key.as_str())
        {
            return Err("unknown append-partition option".into());
        }
        let value = args.get(i + 1).ok_or("missing option value")?;
        if options.insert(key.clone(), value.clone()).is_some() {
            return Err("duplicate option".into());
        }
        i += 2;
    }
    let required = |key: &str| {
        options
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| format!("missing {key}"))
    };
    match command {
        "catalog" => Ok(
            json!({"schema":"agentforge-append-families/v1","families":catalog::ALL,"effects":false}),
        ),
        "snapshot" => {
            let catalog: serde_json::Value = serde_json::from_slice(&append::read_bounded(
                Path::new(required("--catalog")?),
                4 * 1024 * 1024,
            )?)
            .map_err(|_| "catalog JSON invalid")?;
            let authority = append::read_bounded(
                Path::new(required("--authority-snapshot")?),
                16 * 1024 * 1024,
            )?;
            let snapshot = Snapshot {
                schema: "agentforge-append-catalog/v1".into(),
                authority_snapshot_sha256: append::sha256(&authority),
                catalog_sha256: append::sha256(
                    &serde_json::to_vec(&catalog).map_err(|_| "catalog encoding invalid")?,
                ),
                catalog,
            };
            append::validate_snapshot(&snapshot)?;
            serde_json::to_value(snapshot).map_err(|_| "snapshot encoding failed".into())
        }
        "maintenance" => {
            let snapshots: Vec<Snapshot> = serde_json::from_slice(&append::read_bounded(
                Path::new(required("--snapshots")?),
                16 * 1024 * 1024,
            )?)
            .map_err(|_| "maintenance snapshot array invalid")?;
            append::maintenance(
                &snapshots,
                required("--now")?
                    .parse()
                    .map_err(|_| "explicit UTC observation time required")?,
                required("--daily-ahead")?
                    .parse()
                    .map_err(|_| "daily horizon invalid")?,
                required("--monthly-ahead")?
                    .parse()
                    .map_err(|_| "monthly horizon invalid")?,
            )
        }
        "plan" => {
            let snapshot = append::read_snapshot(Path::new(required("--snapshot")?))?;
            let operation: Operation = serde_json::from_slice(&append::read_bounded(
                Path::new(required("--operation")?),
                4 * 1024 * 1024,
            )?)
            .map_err(|_| "operation JSON invalid")?;
            let now = required("--now")?
                .parse()
                .map_err(|_| "explicit UTC observation time required")?;
            append::plan(&snapshot, &operation, now)
        }
        "verify-archive" => {
            let manifest: append::ArchiveManifest = serde_json::from_slice(&append::read_bounded(
                Path::new(required("--manifest")?),
                16 * 1024 * 1024,
            )?)
            .map_err(|_| "archive manifest invalid")?;
            let receipt: append::ArchiveReceipt = serde_json::from_slice(&append::read_bounded(
                Path::new(required("--receipt")?),
                65536,
            )?)
            .map_err(|_| "archive receipt invalid")?;
            manifest.verify(&receipt)?;
            Ok(
                json!({"verified":true,"scope":"local-archive-bytes-and-metadata-consistency","effects":false,"live_restore_acceptance":false}),
            )
        }
        _ => Err("unknown append-partition command".into()),
    }
}
fn main() {
    match run() {
        Ok(value) => println!("{}", serde_json::to_string_pretty(&value).unwrap()),
        Err(error) => {
            eprintln!("append partition operation refused: {error}");
            std::process::exit(2);
        }
    }
}
