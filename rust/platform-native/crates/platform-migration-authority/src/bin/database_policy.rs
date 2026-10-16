use ai_platform_migration_authority::{AuthorityError, Result, source_tools::policy};
use std::{collections::BTreeMap, path::PathBuf};
fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut options = BTreeMap::new();
    let mut write = false;
    let mut pending = false;
    while let Some(a) = args.next() {
        if a == "--write" {
            write = true;
            continue;
        }
        if a == "--pending-live-freeze" {
            pending = true;
            continue;
        }
        if a == "--help" {
            println!(
                "database-policy (--source-rev SHA | --pending-live-freeze) --output-dir DIR [--root DIR] [--write]"
            );
            return Ok(());
        }
        if !["--source-rev", "--output-dir", "--root"].contains(&a.as_str()) {
            return Err(AuthorityError::Refused("unknown option".into()));
        }
        options.insert(
            a,
            args.next()
                .ok_or_else(|| AuthorityError::Refused("missing option value".into()))?,
        );
    }
    if pending == options.contains_key("--source-rev") {
        return Err(AuthorityError::Refused(
            "specify exactly one source revision or pending mode".into(),
        ));
    }
    let root = options
        .get("--root")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.."));
    let output = PathBuf::from(
        options
            .get("--output-dir")
            .ok_or_else(|| AuthorityError::Refused("output directory required".into()))?,
    );
    let (inventory, ownership, grants) = policy::build(
        &root,
        options.get("--source-rev").map(String::as_str),
        pending,
    )?;
    if write {
        std::fs::create_dir_all(&output)?;
    }
    for (name, value) in [
        ("data-access-inventory.json", inventory),
        ("ownership-policy.json", ownership),
        ("grants-policy.json", grants),
    ] {
        let content = policy::serialized(&value)?;
        let path = output.join(name);
        if write {
            std::fs::write(path, content)?;
        } else if std::fs::read(path)? != content {
            return Err(AuthorityError::Refused(format!("DRIFT {name}")));
        }
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("BLOCKED: {error}");
        std::process::exit(2);
    }
}
