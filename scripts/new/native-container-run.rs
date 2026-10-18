use std::{
    fs,
    path::{Path, PathBuf},
};
pub fn binary(args: &[String]) -> Result<(String, bool, Vec<String>), String> {
    if args.first().map(String::as_str) != Some("--binary") {
        return Err("run/install requires --binary NAME".into());
    }
    let name = args.get(1).ok_or("binary name missing")?.clone();
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
    {
        return Err("binary must be a plain Cargo executable name".into());
    }
    let mut index = 2;
    let mut release = false;
    if args.get(index).map(String::as_str) == Some("--release") {
        release = true;
        index += 1;
    }
    if args.get(index).map(String::as_str) != Some("--") {
        return Err("run requires -- before binary arguments".into());
    }
    Ok((name, release, args[index + 1..].to_vec()))
}
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
pub fn install(root: &Path, args: &[String]) -> Result<(), String> {
    let marker = args
        .iter()
        .position(|a| a == "--prefix")
        .ok_or("install requires --prefix ABSOLUTE_EXTERNAL_DIRECTORY")?;
    if marker + 2 != args.len() {
        return Err("install accepts --binary NAME [--release] --prefix DIR".into());
    }
    let mut parsed = args[..marker].to_vec();
    parsed.push("--".into());
    let (name, release, _) = binary(&parsed)?;
    let prefix = PathBuf::from(&args[marker + 1]);
    if !prefix.is_absolute() {
        return Err("install prefix must be absolute".into());
    }
    fs::create_dir_all(prefix.join("bin")).map_err(|e| e.to_string())?;
    let bin = fs::canonicalize(prefix.join("bin")).map_err(|e| e.to_string())?;
    if bin.starts_with(root) {
        return Err("command shims must stay outside the source checkout".into());
    }
    let path = bin.join(&name);
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || !fs::read_to_string(&path)
                .unwrap_or_default()
                .starts_with("#!/bin/sh\n# AgentForge Docker native command shim\n")
        {
            return Err(
                "existing command is not an AgentForge Docker shim; choose an unused prefix".into(),
            );
        }
    }
    let launcher = root.join("scripts/new/native-container.sh");
    let body = format!(
        "#!/bin/sh\n# AgentForge Docker native command shim\nexec {} run --binary {} {} -- \"$@\"\n",
        quote(launcher.to_str().ok_or("non-UTF8 root")?),
        quote(&name),
        if release { "--release" } else { "" }
    );
    fs::write(&path, body).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    println!(
        "installed Docker command shim {} (no host ELF/Mach-O copied; build the named binary first)",
        path.display()
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binary_cannot_escape_docker_target_or_inject_environment() {
        for name in ["../gateway", "/bin/sh", "x y", "TOKEN=x", ""] {
            assert!(binary(&["--binary".into(), name.into(), "--".into()]).is_err());
        }
    }
    #[test]
    fn run_preserves_application_args_as_separate_arguments() {
        let args = [
            "--binary",
            "platform-tools",
            "--release",
            "--",
            "gate",
            "argument with spaces",
        ]
        .map(str::to_owned);
        let (name, release, tail) = binary(&args).unwrap();
        assert_eq!(name, "platform-tools");
        assert!(release);
        assert_eq!(tail, ["gate", "argument with spaces"]);
    }
}

// Only these public generated inputs may override the compiler role's read-only
// source mounts. Never widen a parent to make a missing output directory work.
const WORKLOAD_PROJECTIONS: &[&str] = &[
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/src/shared_workload_identity",
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/src/shared_data_connection",
];
const PLUGIN_PROJECTIONS: &[&str] = &[
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-plugin-quiz",
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-python-compat",
];

#[derive(Debug, PartialEq, Eq)]
pub struct SourceMount {
    pub host: PathBuf,
    pub container: String,
}

fn existing_directory(root: &Path, relative: &str) -> Result<PathBuf, String> {
    if !root.is_absolute() || fs::canonicalize(root).map_err(|e| e.to_string())? != root {
        return Err("source projection root must be the canonical repository directory".into());
    }
    let mut current = root.to_path_buf();
    // Check every descendant, not just the leaf: a parent symlink can otherwise
    // turn an apparently scoped bind into a writable path outside the checkout.
    for component in Path::new(relative).components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err("invalid generated source directory".into());
        }
        current.push(component);
        let metadata = fs::symlink_metadata(&current).map_err(|e| {
            format!(
                "generated directory must already exist: {}: {e}",
                current.display()
            )
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(format!(
                "generated directory and parents must be non-symlink directories: {}",
                current.display()
            ));
        }
    }
    if fs::canonicalize(&current).map_err(|e| e.to_string())? != current {
        return Err("generated source directory escaped its canonical path".into());
    }
    // --volume uses colon delimiters. Keep the entire bind one unambiguous argv.
    if current
        .to_str()
        .is_none_or(|s| s.contains(':') || s.chars().any(char::is_control))
    {
        return Err("generated source path cannot contain volume delimiters or controls".into());
    }
    Ok(current)
}

/// Classify raw application argv before granting any host write mount. Other
/// programs retain their ordinary read-only run behavior; a source-lock apply
/// request has only the two exact forms below, with no output/option passthrough.
pub fn source_mounts(
    root: &Path,
    binary: Option<&(String, bool, Vec<String>)>,
) -> Result<Vec<SourceMount>, String> {
    let Some((name, _, args)) = binary else {
        return Ok(Vec::new());
    };
    if name != "platform-tools" || args.first().map(String::as_str) != Some("source-lock") {
        return Ok(Vec::new());
    }
    let directories = match args.get(1).map(String::as_str) {
        Some("sync-workload-source") => WORKLOAD_PROJECTIONS,
        Some("sync-plugin-source") => PLUGIN_PROJECTIONS,
        _ if args
            .iter()
            .any(|arg| arg == "--apply" || arg.starts_with("--apply=")) =>
        {
            return Err(
                "source-lock --apply is limited to the two exact source sync commands".into(),
            );
        }
        _ => return Ok(Vec::new()),
    };
    if !matches!(args.len(), 4 | 5)
        || args[2] != "--repo-root"
        || !(args[3] == "/source" || root.to_str() == Some(args[3].as_str()))
        || (args.len() == 5 && args[4] != "--apply")
    {
        return Err("source sync requires --repo-root /source (or the same canonical host root), optionally followed only by --apply".into());
    }
    if args.len() == 4 {
        return Ok(Vec::new());
    }
    directories
        .iter()
        .map(|relative| {
            Ok(SourceMount {
                host: existing_directory(root, relative)?,
                container: format!("/source/{relative}"),
            })
        })
        .collect()
}

/// Ensure the command receiving write access is the same fixed cache executable
/// and argv that the launcher classified, after its public root-path mapping.
pub fn verify_source_command(
    root: &Path,
    binary: &(String, bool, Vec<String>),
    command: &[String],
) -> Result<(), String> {
    let (name, release, args) = binary;
    let expected_binary = format!(
        "/native-cache/target/{}/{name}",
        if *release { "release" } else { "debug" }
    );
    let root_text = root.to_str().ok_or("non-UTF8 source root")?;
    let mut expected = vec![expected_binary];
    expected.extend(args.iter().map(|arg| {
        if arg == root_text {
            "/source".into()
        } else {
            arg.clone()
        }
    }));
    if command != expected {
        return Err(
            "source write mount command does not match classified executable and argv".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "native-container-source-tests.rs"]
pub(crate) mod source_tests;
