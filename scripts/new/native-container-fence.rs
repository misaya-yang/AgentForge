//! Reuses the canonical resource-lock controller and its existing owner receipts.
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};
pub fn extract(args: &mut Vec<String>) -> Result<(Option<PathBuf>, bool), String> {
    let mut controller = None;
    let mut internal = false;
    let mut index = 0;
    while index < args.len() && args[index] != "--" {
        if args[index] == "--lock-controller" {
            if controller.is_some() {
                return Err("duplicate --lock-controller".into());
            }
            let path = PathBuf::from(
                args.get(index + 1)
                    .ok_or("--lock-controller requires ABS_PATH")?,
            );
            controller = Some(validate_controller(&path)?);
            args.drain(index..index + 2);
        } else if args[index] == "--resource-lock-child" {
            if internal {
                return Err("duplicate internal lock entry".into());
            }
            internal = true;
            args.remove(index);
        } else {
            index += 1;
        }
    }
    Ok((controller, internal))
}
fn validate_controller(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("lock-controller path must be absolute".into());
    }
    let path = fs::canonicalize(path)
        .map_err(|e| format!("bootstrap lock-controller unavailable: {e}"))?;
    let metadata = fs::metadata(&path).map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("bootstrap controller must be a regular native executable".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err("bootstrap lock-controller is not executable".into());
        }
    }
    let mut header = [0u8; 4];
    fs::File::open(&path)
        .map_err(|e| e.to_string())?
        .read_exact(&mut header)
        .map_err(|e| e.to_string())?;
    let native = header == [0x7f, b'E', b'L', b'F']
        || [
            [0xcf, 0xfa, 0xed, 0xfe],
            [0xce, 0xfa, 0xed, 0xfe],
            [0xfe, 0xed, 0xfa, 0xcf],
            [0xfe, 0xed, 0xfa, 0xce],
            [0xca, 0xfe, 0xba, 0xbe],
            [0xca, 0xfe, 0xba, 0xbf],
            [0xbe, 0xba, 0xfe, 0xca],
            [0xbf, 0xba, 0xfe, 0xca],
        ]
        .contains(&header);
    if !native {
        return Err("lock-controller must be the retained native bootstrap binary, never a shell/Docker shim".into());
    }
    Ok(path)
}
pub fn configured() -> Result<Option<PathBuf>, String> {
    let path = crate::policy::external_cache()?.join("lock-controller.path");
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(metadata)
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= 4096 => {}
        _ => return Err("lock-controller.path must be a small regular public path config".into()),
    }
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let lines: Vec<_> = text.lines().collect();
    if lines.len() != 1 || lines[0].is_empty() || lines[0].chars().any(char::is_control) {
        return Err("lock-controller.path requires exactly one absolute executable path".into());
    }
    validate_controller(Path::new(lines[0])).map(Some)
}
pub fn heavy(action: &str) -> bool {
    [
        "prepare",
        "check",
        "test",
        "build",
        "engine-build",
        "engine-fetch",
    ]
    .contains(&action)
}
pub fn outer_arguments(
    root: &Path,
    controller: &Path,
    executable: &Path,
    args: &[String],
) -> Vec<String> {
    let mut output=vec!["resource-lock".into(),"run".into(),"--root".into(),root.to_string_lossy().into_owned(),"--resource".into(),"rust-build".into(),"--expected-end-condition".into(),"Native container command exits; ephemeral compiler process completes; canonical rust-build/low-memory fences released by controller".into(),"--".into(),executable.to_string_lossy().into_owned(),"--root".into(),root.to_string_lossy().into_owned(),"--lock-controller".into(),controller.to_string_lossy().into_owned(),"--resource-lock-child".into()];
    output.extend_from_slice(args);
    output
}
pub fn check_controller(root: &Path, controller: &Path) -> Result<(), String> {
    let probe = Command::new(controller)
        .current_dir(root)
        .args(["resource-lock", "status", "--root"])
        .arg(root)
        .output()
        .map_err(|e| e.to_string())?;
    if !probe.status.success() {
        return Err("bootstrap controller does not expose canonical resource-lock status".into());
    }
    let probe = String::from_utf8(probe.stdout).map_err(|e| e.to_string())?;
    member(&probe, "rust-build")?;
    member(&probe, ".low-memory")?;
    Ok(())
}
pub fn delegate(root: &Path, controller: &Path, args: &[String]) -> Result<u8, String> {
    check_controller(root, controller)?;
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let status = Command::new(controller)
        .current_dir(root)
        .args(outer_arguments(root, controller, &executable, args))
        .status()
        .map_err(|e| format!("resource-lock controller: {e}"))?;
    Ok(status.code().unwrap_or(1).clamp(0, 255) as u8)
}
// Extract members from the trusted controller's JSON output without a dependency or
// accepting substring matches inside quoted command/identity fields.
fn end(value: &str, start: usize) -> Result<usize, String> {
    let bytes = value.as_bytes();
    let mut index = start;
    let mut depth = 0usize;
    let mut string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let c = bytes[index];
        if string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                string = false;
                if depth == 0 {
                    return Ok(index + 1);
                }
            }
        } else {
            match c {
                b'"' => string = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > 64 {
                        return Err("lock JSON depth".into());
                    }
                }
                b'}' | b']' => {
                    if depth == 0 {
                        return Ok(index);
                    }
                    depth -= 1;
                    if depth == 0 {
                        return Ok(index + 1);
                    }
                }
                b',' if depth == 0 => return Ok(index),
                _ => {}
            }
        }
        index += 1;
    }
    if string || depth > 0 {
        Err("truncated lock JSON".into())
    } else {
        Ok(index)
    }
}
fn member<'a>(object: &'a str, key: &str) -> Result<&'a str, String> {
    let object = object.trim();
    if !object.starts_with('{') || !object.ends_with('}') {
        return Err("lock receipt must be object".into());
    }
    let mut index = 1;
    let bytes = object.as_bytes();
    while index < bytes.len() - 1 {
        while bytes
            .get(index)
            .is_some_and(|c| c.is_ascii_whitespace() || *c == b',')
        {
            index += 1;
        }
        if index >= bytes.len() - 1 {
            break;
        }
        if bytes[index] != b'"' {
            return Err("invalid lock JSON member".into());
        }
        let key_end = end(object, index)?;
        let field = &object[index..key_end];
        index = key_end;
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        if bytes.get(index) != Some(&b':') {
            return Err("invalid lock JSON separator".into());
        }
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        let value_end = end(object, index)?;
        if field == crate::json(key) {
            return Ok(object[index..value_end].trim());
        }
        index = value_end;
    }
    Err(format!("required canonical lock field absent: {key}"))
}
fn receipts(status: &str, parent: u32, root: &Path) -> Result<(), String> {
    let resource = member(status, "rust-build")?;
    let mutex = member(status, ".low-memory")?;
    for owner in [resource, mutex] {
        if member(owner, "schema_version")? != crate::json("ai-gateway/resource-lock/v1")
            || member(owner, "pid")?.parse::<u32>().ok() != Some(parent)
            || member(owner, "worktree_path")? != crate::json(root.to_str().ok_or("non-UTF8 root")?)
        {
            return Err("internal entry is not a direct child of this checkout's canonical rust-build owner".into());
        }
    }
    if member(resource, "resource")? != crate::json("rust-build")
        || member(resource, "owner_token")? != member(mutex, "owner_token")?
        || member(resource, "owner_token")? == "\"\""
    {
        return Err("canonical resource/mutex owner mismatch".into());
    }
    Ok(())
}
pub fn verify(root: &Path, controller: &Path) -> Result<(), String> {
    let pid = std::process::id().to_string();
    let parent = Command::new("ps")
        .args(["-p", &pid, "-o", "ppid="])
        .output()
        .map_err(|e| e.to_string())?;
    if !parent.status.success() {
        return Err("cannot verify canonical lock parent".into());
    }
    let parent = String::from_utf8(parent.stdout)
        .map_err(|e| e.to_string())?
        .trim()
        .parse::<u32>()
        .map_err(|e| e.to_string())?;
    let status = Command::new(controller)
        .current_dir(root)
        .args(["resource-lock", "status", "--root"])
        .arg(root)
        .output()
        .map_err(|e| e.to_string())?;
    if !status.status.success() {
        return Err("canonical resource-lock status unavailable".into());
    }
    receipts(
        &String::from_utf8(status.stdout).map_err(|e| e.to_string())?,
        parent,
        root,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn heavy_commands_use_one_existing_controller_fence() {
        for action in [
            "prepare",
            "check",
            "test",
            "build",
            "engine-build",
            "engine-fetch",
        ] {
            assert!(heavy(action));
        }
        for action in ["plan", "doctor", "install", "watch"] {
            assert!(!heavy(action));
        }
        let args = outer_arguments(
            Path::new("/repo"),
            Path::new("/bootstrap/platform-tools"),
            Path::new("/launcher"),
            &["test".into()],
        );
        assert_eq!(&args[..2], ["resource-lock", "run"]);
        assert!(args.windows(2).any(|v| v == ["--resource", "rust-build"]));
        assert!(args.iter().any(|a| a == "--resource-lock-child"));
    }
    #[test]
    fn ambient_flags_cannot_establish_a_lock_owner() {
        let owner = "{\"schema_version\":\"ai-gateway/resource-lock/v1\",\"pid\":42,\"worktree_path\":\"/repo\",\"resource\":\"rust-build\",\"owner_token\":\"real-owner\"}";
        let status = format!("{{\"rust-build\":{owner},\".low-memory\":{owner}}}");
        receipts(&status, 42, Path::new("/repo")).unwrap();
        assert!(receipts(&status, 41, Path::new("/repo")).is_err());
        assert!(receipts(&status, 42, Path::new("/other")).is_err());
        assert!(receipts("{}", 42, Path::new("/repo")).is_err());
        let mismatch = status.replacen("real-owner", "foreign", 1);
        assert!(receipts(&mismatch, 42, Path::new("/repo")).is_err());
    }
    #[test]
    fn strings_cannot_spoof_receipt_members() {
        let status = r#"{"command":"fake: \\\"pid\\\":42", "pid":123,"nested":{"pid":42}}"#;
        assert_eq!(member(status, "pid").unwrap(), "123");
        assert!(member(status, "absent").is_err());
    }
    #[test]
    fn controller_must_be_native_not_a_docker_or_shell_shim() {
        assert!(validate_controller(Path::new("relative-controller")).is_err());
        let path =
            std::env::temp_dir().join(format!("agentforge-controller-shim-{}", std::process::id()));
        fs::write(&path, "#!/bin/sh\nexec docker run ...\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(validate_controller(&path).unwrap_err().contains("shim"));
        fs::remove_file(path).unwrap();
    }
}
