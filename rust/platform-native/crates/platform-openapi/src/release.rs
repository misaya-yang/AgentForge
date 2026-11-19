//! Versioned release producer and fail-closed, offline PR compatibility gate.
use serde_json::{Value, json};
use std::{fs, io::Write, path::Path};
fn read(path: &str) -> Result<Value, String> {
    serde_json::from_slice(&fs::read(path).map_err(|e| format!("read {path}: {e}"))?)
        .map_err(|e| format!("JSON {path}: {e}"))
}
fn value(args: &[String], name: &str) -> Result<String, String> {
    let index = args
        .iter()
        .position(|a| a == name)
        .ok_or_else(|| format!("required {name}"))?;
    args.get(index + 1)
        .filter(|v| !v.starts_with("--"))
        .cloned()
        .ok_or_else(|| format!("required value for {name}"))
}
fn options(args: &[String], valued: &[&str], flags: &[&str]) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        if !seen.insert(key) {
            return Err(format!("duplicate option {key}"));
        }
        if valued.contains(&key) {
            value(args, key)?;
            index += 2;
        } else if flags.contains(&key) {
            index += 1;
        } else {
            return Err(format!("unknown option {key}"));
        }
    }
    Ok(())
}
pub fn run(args: &[String], produced: Option<Value>) -> u8 {
    match execute(args, produced) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("OpenAPI release: {e}");
            2
        }
    }
}
fn execute(args: &[String], produced: Option<Value>) -> Result<u8, String> {
    let command = args
        .first()
        .ok_or("required produce, validate, or breaking")?;
    let args = &args[1..];
    match command.as_str() {
        "produce" => {
            options(args, &["--release-version", "--out"], &[])?;
            let version = value(args, "--release-version")?;
            crate::breaking::major(&version)?;
            let mut document = produced.ok_or("native producer not supplied")?;
            document["info"]["version"] = json!(version);
            crate::validation::validate(&document)?;
            let output = value(args, "--out")?;
            let path = Path::new(&output);
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
            }
            // Released baselines are immutable; creating a release never overwrites one.
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|e| e.to_string())?;
            file.write_all(
                format!(
                    "{}\n",
                    serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?
                )
                .as_bytes(),
            )
            .map_err(|e| e.to_string())?;
            Ok(0)
        }
        "validate" => {
            options(args, &["--candidate"], &[])?;
            crate::validation::validate(&read(&value(args, "--candidate")?)?)?;
            println!("official OpenAPI 3.2 schema-base: valid (offline)");
            Ok(0)
        }
        "breaking" => {
            options(
                args,
                &["--baseline", "--candidate"],
                &["--allow-major-breaking"],
            )?;
            let base = read(&value(args, "--baseline")?)?;
            let candidate = read(&value(args, "--candidate")?)?;
            crate::validation::validate(&base)?;
            crate::validation::validate(&candidate)?;
            let allow = args.iter().any(|a| a == "--allow-major-breaking");
            let issues = crate::breaking::authorize(&base, &candidate, allow)?;
            let failed = !issues.is_empty() && !allow;
            println!(
                "{}",
                json!({"gate":"openapi-release-breaking","result":if failed{"blocked"}else{"pass"},"major_breaking_authorized":allow&&!issues.is_empty(),"changes":issues,"comparison":"conservative; changed assertions require major release review","runtime_verified":false})
            );
            Ok(if failed { 1 } else { 0 })
        }
        _ => Err("required produce, validate, or breaking".into()),
    }
}
