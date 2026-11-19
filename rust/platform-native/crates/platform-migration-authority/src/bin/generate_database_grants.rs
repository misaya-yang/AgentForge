use ai_platform_migration_authority::{
    AuthorityError, DEFAULT_ROLE_PREFIX, Result, source_tools::grants,
};
use std::{collections::BTreeMap, path::PathBuf};
fn run() -> Result<()> {
    let mut options = BTreeMap::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--help" {
            println!(
                "generate-database-grants --inventory FILE --policy FILE (--output FILE | --check FILE)"
            );
            return Ok(());
        }
        if !["--inventory", "--policy", "--output", "--check"].contains(&a.as_str()) {
            return Err(AuthorityError::Refused("unknown option".into()));
        }
        options.insert(
            a,
            args.next()
                .ok_or_else(|| AuthorityError::Refused("missing option value".into()))?,
        );
    }
    if options.contains_key("--output") == options.contains_key("--check") {
        return Err(AuthorityError::Refused(
            "exactly one output/check target required".into(),
        ));
    }
    let read = |key: &str| -> Result<Vec<u8>> {
        Ok(std::fs::read(options.get(key).ok_or_else(|| {
            AuthorityError::Refused(format!("{key} required"))
        })?)?)
    };
    let rendered = grants::generate(
        &read("--inventory")?,
        &read("--policy")?,
        DEFAULT_ROLE_PREFIX,
    )?;
    if let Some(path) = options.get("--check") {
        if std::fs::read_to_string(path)? != rendered {
            return Err(AuthorityError::Refused(
                "grants differ from deterministic inventory+policy output".into(),
            ));
        }
    } else {
        let path = PathBuf::from(&options["--output"]);
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, rendered)?;
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("BLOCKED: {error}");
        std::process::exit(2);
    }
}
