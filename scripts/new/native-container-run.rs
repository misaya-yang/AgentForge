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
