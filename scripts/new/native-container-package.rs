//! Package existing Linux executables; never invoke Cargo or mount source into export.
use crate::{docker, policy};
use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Debug, PartialEq, Eq)]
struct Selection {
    component: String,
    binary: &'static str,
    variable: &'static str,
    output: PathBuf,
    tag: String,
    build: bool,
}
fn parse(args: &[String]) -> Result<Selection, String> {
    let mut component = None;
    let mut output = None;
    let mut tag = None;
    let mut build = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--component" if component.is_none() => { component=Some(args.get(i+1).ok_or("missing component")?.clone()); i+=2; },
            "--output" if output.is_none() => { output=Some(PathBuf::from(args.get(i+1).ok_or("missing output")?)); i+=2; },
            "--tag" if tag.is_none() => { tag=Some(args.get(i+1).ok_or("missing tag")?.clone()); i+=2; },
            "--build" if !build => { build=true; i+=1; },
            _ => return Err("package requires --component gateway|knowledge|migrate|tools|credentials|auth-bootstrap --output NEW_ABSOLUTE_EXTERNAL_DIRECTORY --tag LOCAL_TAG [--build]; no other options".into()),
        }
    }
    let component = component.ok_or("package needs explicit component")?;
    let (binary, variable) = match component.as_str() {
        "gateway" => ("gateway", "GATEWAY_IMAGE"),
        "knowledge" => ("platform-knowledge-service", "KNOWLEDGE_IMAGE"),
        "migrate" => ("database-authority", "MIGRATE_IMAGE"),
        "tools" => ("platform-tools", "LOCAL_FRESH_TOOLS_IMAGE"),
        "credentials" => ("workload-credentials", "WORKLOAD_CREDENTIALS_IMAGE"),
        "auth-bootstrap" => ("admin-create", "ADMIN_BOOTSTRAP_IMAGE"),
        _ => return Err("component is outside native development image allowlist".into()),
    };
    let tag = tag.ok_or("package needs explicit local tag")?;
    // Fixed local namespace prevents accidental replacement of production/upstream tags.
    if !tag.starts_with("agentforge-native-local:")
        || tag.len() > 160
        || tag
            .trim_start_matches("agentforge-native-local:")
            .is_empty()
        || !tag
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-:".contains(&b))
        || tag.matches(':').count() != 1
    {
        return Err("tag must be agentforge-native-local:<lowercase local identity>".into());
    }
    Ok(Selection {
        component,
        binary,
        variable,
        output: output.ok_or("package needs external output")?,
        tag,
        build,
    })
}
fn destination(root: &Path, output: &Path) -> Result<(), String> {
    if !output.is_absolute()
        || output
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        || output.starts_with(root)
        || root.starts_with(output)
    {
        return Err(
            "output must be a new normalized absolute directory outside the repository".into(),
        );
    }
    let mut ancestor = PathBuf::new();
    for part in output.components() {
        ancestor.push(part);
        if ancestor == output {
            break;
        }
        let meta = fs::symlink_metadata(&ancestor)
            .map_err(|e| format!("output parent {}: {e}", ancestor.display()))?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(
                "output parents must be existing real directories, without symlinks".into(),
            );
        }
    }
    match fs::symlink_metadata(output) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        _ => {
            return Err(
                "output must not already exist; retained exports are never overwritten".into(),
            );
        }
    }
    let text = output.to_str().ok_or("output path must be UTF8")?;
    if text.chars().any(char::is_control) || text.contains([',', ':']) {
        return Err("output contains Docker mount delimiters or control characters".into());
    }
    Ok(())
}
fn command() -> Command {
    let mut c = Command::new("docker");
    c.env_clear();
    for key in [
        "PATH",
        "HOME",
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_CONFIG",
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
        "SSH_AUTH_SOCK",
    ] {
        if let Some(value) = std::env::var_os(key) {
            c.env(key, value);
        }
    }
    c
}
fn inspect(args: &[&str]) -> Result<String, String> {
    let result = command().args(args).output().map_err(|e| e.to_string())?;
    if !result.status.success() {
        return Err(format!(
            "read-only Docker inspection failed: {}",
            String::from_utf8_lossy(&result.stderr)
        ));
    }
    Ok(String::from_utf8(result.stdout)
        .map_err(|e| e.to_string())?
        .trim()
        .to_owned())
}
fn export_script(binary: &str) -> String {
    // binary comes exclusively from parse's fixed allowlist, never caller shell input.
    format!(
        "set -eu; for directory in /native-cache/target /native-cache/target/debug; do test -d \"$directory\" && test ! -L \"$directory\"; done; source=/native-cache/target/debug/{binary}; test -f \"$source\" && test ! -L \"$source\" && test -x \"$source\"; test \"$(od -An -tx1 -N4 \"$source\" | tr -d ' \\n')\" = 7f454c46; mkdir /export/bin; cp --no-preserve=ownership \"$source\" /export/bin/{binary}; chmod 0555 /export/bin/{binary}; cd /export; sha256sum bin/{binary} > binaries.sha256; stat -c '%s %Y' \"$source\" > cache-artifact.stat"
    )
}
fn exporter(selection: &Selection, image: &str) -> Result<Command, String> {
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(&selection.output).map_err(|e| e.to_string())?;
        format!("{}:{}", meta.uid(), meta.gid())
    };
    #[cfg(not(unix))]
    let identity = return Err("native development packaging needs Unix ownership".into());
    let mut c = command();
    c.args([
        "run",
        "--rm",
        "--pull=never",
        "--network=none",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--pids-limit=64",
        "--memory=128m",
        "--cpus=1",
        "--user",
        &identity,
        "--mount",
    ])
    .arg(format!(
        "type=volume,src={},dst=/native-cache,readonly",
        policy::VOLUME
    ))
    .arg("--mount")
    .arg(format!(
        "type=bind,src={},dst=/export",
        selection.output.display()
    ))
    .args([
        "--entrypoint",
        "/bin/sh",
        image,
        "-ec",
        &export_script(selection.binary),
    ]);
    Ok(c)
}
fn files(path: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if meta.is_file() {
        out.push(path.to_owned());
    } else if meta.is_dir() {
        for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
            files(&entry.map_err(|e| e.to_string())?.path(), out)?;
        }
    } else {
        return Err("nonregular public source input".into());
    }
    Ok(())
}
fn source_sha(root: &Path) -> Result<String, String> {
    let mut paths = Vec::new();
    for source in policy::SOURCES {
        let path = root.join(source);
        policy::scan(&path)?;
        files(&path, &mut paths)?;
    }
    paths.sort();
    paths.dedup();
    let mut child = Command::new("shasum")
        .args(["-a", "256"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("source identity needs shasum: {e}"))?;
    let result = (|| -> Result<(), String> {
        let mut stdin = child.stdin.take().ok_or("hash stdin unavailable")?;
        for path in paths {
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .ok_or("source path UTF8")?;
            let mut file = fs::File::open(&path).map_err(|e| e.to_string())?;
            let len = file.metadata().map_err(|e| e.to_string())?.len();
            write!(stdin, "{}:{name}:{len}:", name.len()).map_err(|e| e.to_string())?;
            std::io::copy(&mut file, &mut stdin).map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    let result_hash = child.wait_with_output().map_err(|e| e.to_string())?;
    result?;
    if !result_hash.status.success() {
        return Err("source digest failed".into());
    }
    let value = String::from_utf8(result_hash.stdout).map_err(|e| e.to_string())?;
    let digest = value.split_whitespace().next().unwrap_or("");
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid source digest".into());
    }
    Ok(digest.to_owned())
}
fn copy_public(path: &Path, target: &Path) -> Result<(), String> {
    policy::scan(path)?;
    if path.is_dir() {
        fs::create_dir(target).map_err(|e| e.to_string())?;
        for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            copy_public(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else {
        fs::copy(path, target).map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn build_command(
    selection: &Selection,
    image: &str,
    source: &str,
    revision: &str,
    binary_sha: &str,
) -> Command {
    let mut c = command();
    c.args([
        "build",
        "--pull=false",
        "--network=none",
        "--target",
        &selection.component,
        "--tag",
        &selection.tag,
        "--build-arg",
        &format!("NATIVE_DONOR_IMAGE={}", policy::IMAGE),
        "--build-arg",
        &format!("SOURCE_SHA256={source}"),
        "--build-arg",
        &format!("VCS_REF={revision}"),
        "--label",
        &format!("agentforge.native.binary-sha256={binary_sha}"),
        "--label",
        &format!("agentforge.native.donor-image={image}"),
        "--file",
    ])
    .arg(selection.output.join("Dockerfile"))
    .arg(&selection.output);
    c
}
pub fn run(root: &Path, args: &[String]) -> Result<u8, String> {
    let selection = parse(args)?;
    destination(root, &selection.output)?;
    docker::owners(root)?;
    docker::version_label()?;
    // Unlike a normal first compile, packaging may never create a missing cache volume.
    inspect(&["volume", "inspect", policy::VOLUME, "--format", "{{.Name}}"])?;
    let image = inspect(&["image", "inspect", policy::IMAGE, "--format", "{{.Id}}"])?;
    if !image.starts_with("sha256:")
        || image.len() != 71
        || !image[7..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("invalid immutable donor image identity".into());
    }
    let platform = inspect(&[
        "image",
        "inspect",
        policy::IMAGE,
        "--format",
        "{{.Os}}/{{.Architecture}}",
    ])?;
    let source = source_sha(root)?;
    let git = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| e.to_string())?;
    if !git.status.success() {
        return Err("repository HEAD unavailable".into());
    }
    let revision = String::from_utf8(git.stdout)
        .map_err(|e| e.to_string())?
        .trim()
        .to_owned();
    fs::create_dir(&selection.output).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&selection.output, fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    let status = exporter(&selection, &image)?
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!(
            "cache export failed ({status}); partial export retained at {}",
            selection.output.display()
        ));
    }
    for (from, to) in [
        (
            "deploy/platform-native/dev/Dockerfile.package",
            "Dockerfile",
        ),
        (
            "deploy/platform-native/dev/collect-runtime.sh",
            "collect-runtime.sh",
        ),
        ("LICENSE", "LICENSE"),
        ("NOTICE", "NOTICE"),
    ] {
        copy_public(&root.join(from), &selection.output.join(to))?;
    }
    if !matches!(selection.component.as_str(), "knowledge" | "auth-bootstrap") {
        copy_public(&root.join("database"), &selection.output.join("database"))?;
    }
    fs::write(selection.output.join(".dockerignore"),"**\n!Dockerfile\n!collect-runtime.sh\n!bin/\n!bin/**\n!database/\n!database/**\n!source-identity.json\n!binaries.sha256\n!LICENSE\n!NOTICE\n").map_err(|e|e.to_string())?;
    if source_sha(root)? != source {
        return Err("public source changed during export; retained context is not admitted; repeat into a fresh directory".into());
    }
    let manifest =
        fs::read_to_string(selection.output.join("binaries.sha256")).map_err(|e| e.to_string())?;
    let binary_sha = manifest
        .split_whitespace()
        .next()
        .ok_or("missing binary SHA")?;
    if binary_sha.len() != 64
        || !binary_sha.bytes().all(|b| b.is_ascii_hexdigit())
        || manifest.split_whitespace().nth(1) != Some(&format!("bin/{}", selection.binary))
        || manifest.split_whitespace().count() != 2
    {
        return Err("invalid exported binary SHA manifest".into());
    }
    let receipt = format!(
        "{{\"schema\":\"agentforge/native-dev-package/v1\",\"component\":{},\"binary\":{},\"binary_sha256\":{},\"observed_source_sha256\":{},\"source_scope\":\"native-container public source allowlist; length-prefixed paths and bytes\",\"observed_head\":{},\"build_source_binding\":\"unattested-existing-cache; observed source is not a build attestation\",\"donor_image\":{},\"platform\":{},\"cache_volume\":{},\"profile\":\"debug\",\"release_acceptance\":false}}\n",
        crate::json(&selection.component),
        crate::json(selection.binary),
        crate::json(binary_sha),
        crate::json(&source),
        crate::json(&revision),
        crate::json(&image),
        crate::json(&platform),
        crate::json(policy::VOLUME)
    );
    fs::write(selection.output.join("source-identity.json"), receipt).map_err(|e| e.to_string())?;
    println!(
        "exported {} SHA256={binary_sha} source={source} to {}",
        selection.binary,
        selection.output.display()
    );
    if selection.build {
        if inspect(&["image", "inspect", policy::IMAGE, "--format", "{{.Id}}"])? != image {
            return Err("donor tag changed after export; refusing package build".into());
        }
        let status = build_command(&selection, &image, &source, &revision, binary_sha)
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Ok(status.code().unwrap_or(1).clamp(1, 255) as u8);
        }
        fs::write(
            selection.output.join("image.env"),
            format!("{}={}\n", selection.variable, selection.tag),
        )
        .map_err(|e| e.to_string())?;
        let id = inspect(&["image", "inspect", &selection.tag, "--format", "{{.Id}}"])?;
        fs::write(selection.output.join("built-image.id"), format!("{id}\n"))
            .map_err(|e| e.to_string())?;
        println!(
            "built {} ({id}); image.env contains only the admitted image variable; no services started",
            selection.tag
        );
    } else {
        println!(
            "export only; repeat with a fresh --output and --build to build the image (no Cargo). No image admission emitted."
        );
    }
    Ok(0)
}
#[cfg(test)]
#[path = "native-container-package-tests.rs"]
mod tests;
