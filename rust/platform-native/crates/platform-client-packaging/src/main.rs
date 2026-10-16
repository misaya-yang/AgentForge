//! Portable client packaging entrypoint. No server operations or Unix resource locks.
#[path = "../../platform-tools/src/packaging.rs"]
mod packaging;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn require(ok: bool, message: impl Into<String>) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.into().into())
    }
}
include!("support.rs");
#[derive(Default)]
struct Args {
    values: BTreeMap<String, String>,
    flags: Vec<String>,
    positionals: Vec<String>,
}
impl Args {
    fn parse() -> Result<Self> {
        let mut parsed = Self::default();
        let mut args = std::env::args().skip(1);
        while let Some(value) = args.next() {
            match value.as_str() {
                "--matrix" | "--build-env" => parsed.flags.push(value),
                "--repo-root" | "--target" | "--binary" | "--output" => {
                    let argument = args.next().ok_or("missing option value")?;
                    require(!argument.starts_with("--"), "missing option value")?;
                    require(
                        parsed.values.insert(value, argument).is_none(),
                        "duplicate option",
                    )?;
                }
                _ if value.starts_with('-') => return Err(format!("unknown option {value}").into()),
                _ => parsed.positionals.push(value),
            }
        }
        require(
            parsed.positionals.len() == 1,
            "expected package-desktop or package-platformctl",
        )?;
        Ok(parsed)
    }
    fn get(&self, key: &str) -> Result<&str> {
        self.values
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| format!("missing {key}").into())
    }
    fn flag(&self, key: &str) -> bool {
        self.flags.iter().any(|s| s == key)
    }
    fn root(&self) -> PathBuf {
        self.values
            .get("--repo-root")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    }
}
fn run() -> Result<()> {
    let args = Args::parse()?;
    let desktop = match args.positionals[0].as_str() {
        "package-desktop" => true,
        "package-platformctl" => false,
        _ => return Err("unknown packaging command".into()),
    };
    let result = packaging::run(&args, desktop)?;
    if let Some(plain) = result["_plain"].as_str() {
        println!("{plain}")
    } else {
        println!("{}", serde_json::to_string_pretty(&result)?)
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1)
    }
}
