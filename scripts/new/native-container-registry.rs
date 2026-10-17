use std::{
    fs,
    path::{Path, PathBuf},
};
/// Remove only launcher-level options; arguments after `--` belong to Cargo tests/binaries.
pub fn extract(args: &mut Vec<String>) -> Result<Option<PathBuf>, String> {
    let limit = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let positions: Vec<_> = args[..limit]
        .iter()
        .enumerate()
        .filter(|(_, a)| a.as_str() == "--public-registry-source")
        .map(|(i, _)| i)
        .collect();
    if positions.len() > 1 {
        return Err("duplicate --public-registry-source".into());
    }
    let Some(index) = positions.first().copied() else {
        return Ok(None);
    };
    let value = args
        .get(index + 1)
        .filter(|s| !s.starts_with('-'))
        .ok_or("--public-registry-source requires a directory")?
        .clone();
    args.drain(index..index + 2);
    let path =
        fs::canonicalize(value).map_err(|e| format!("public directory-source unavailable: {e}"))?;
    validate(&path)?;
    Ok(Some(path))
}
pub fn validate(path: &Path) -> Result<(), String> {
    if !path.is_dir() {
        return Err("public registry source must be a directory".into());
    }
    let mut count = 0;
    for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        // Never mount ~/.cargo itself, credentials/config, tar caches, or a mixed private directory.
        if !kind.is_dir()
            || kind.is_symlink()
            || !entry.path().join("Cargo.toml").is_file()
            || !entry.path().join(".cargo-checksum.json").is_file()
        {
            return Err("public registry source must contain only extracted checksummed Cargo package directories; do not select ~/.cargo".into());
        }
        count += 1;
    }
    if count == 0 {
        return Err("public registry source is empty".into());
    }
    Ok(())
}
pub fn configure(args: &mut Vec<String>) {
    // Global Cargo options precede the subcommand and the test-harness `--` delimiter.
    args.splice(
        0..0,
        [
            "--config".into(),
            "source.crates-io.replace-with=\"af-public-registry\"".into(),
            "--config".into(),
            "source.af-public-registry.directory=\"/af-public-registry\"".into(),
        ],
    );
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacement_is_explicit_and_cannot_become_test_harness_arguments() {
        let mut args = vec![
            "test".into(),
            "--offline".into(),
            "--".into(),
            "filter".into(),
        ];
        configure(&mut args);
        assert_eq!(args[0], "--config");
        assert_eq!(args[4], "test");
        assert_eq!(args[6], "--");
        assert!(args[1].contains("replace-with"));
        assert!(args[3].contains("/af-public-registry"));
    }
    #[test]
    fn rejects_empty_or_non_package_roots_and_duplicate_options() {
        let path =
            std::env::temp_dir().join(format!("agentforge-registry-test-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        assert!(validate(&path).is_err());
        fs::write(path.join("credentials.toml"), "synthetic fixture only").unwrap();
        assert!(validate(&path).is_err());
        fs::remove_dir_all(path).unwrap();
        let mut args = [
            "check",
            "--public-registry-source",
            "a",
            "--public-registry-source",
            "b",
        ]
        .map(str::to_owned)
        .to_vec();
        assert!(extract(&mut args).is_err());
    }
}
