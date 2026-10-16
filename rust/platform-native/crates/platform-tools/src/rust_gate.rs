//! Hosted-only, lock-pinned changed-crate execution for native and composed workspaces.
use crate::*;
use std::{collections::BTreeSet, os::unix::fs::PermissionsExt, process::Command};
const OVERLAY: &str = "rust/agent-runtime-overlay/kernel-rs";
const NATIVE: &str = "rust/platform-native";
fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(["--no-replace-objects", "-C"])
        .arg(root)
        .args(args)
        .output()?;
    require(
        output.status.success(),
        "pinned source Git operation failed",
    )?;
    Ok(String::from_utf8(output.stdout)?)
}
fn hosted() -> bool {
    std::env::var("CI").is_ok_and(|v| v.eq_ignore_ascii_case("true"))
        && (std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v.eq_ignore_ascii_case("true"))
            || std::env::var("AI_PLATFORM_RUST_GATE_HOSTED_CI").as_deref() == Ok("1"))
}
fn plan(root: &Path, paths: &[String]) -> Result<BTreeMap<(String, String), Vec<String>>> {
    let mut selected = BTreeMap::<(String, String), Vec<String>>::new();
    for path in paths {
        if path == "deploy/agent-runtime-source/lock.json"
            || path == "rust/agent-runtime-overlay/manifest.json"
        {
            selected
                .entry((OVERLAY.into(), "@workspace".into()))
                .or_default()
                .push(path.clone());
            continue;
        }
        if !path.starts_with("rust/") {
            continue;
        }
        let ws = if path.starts_with(&format!("{NATIVE}/")) {
            NATIVE
        } else if path.starts_with(&format!("{OVERLAY}/")) {
            OVERLAY
        } else {
            return Err(format!("Rust change maps to no Cargo crate: {path}").into());
        };
        let relative = path.strip_prefix(&format!("{ws}/")).unwrap();
        if ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"].contains(&relative) {
            selected
                .entry((ws.into(), "@workspace".into()))
                .or_default()
                .push(path.clone());
            continue;
        }
        let mut directory = root
            .join(path)
            .parent()
            .ok_or("invalid changed path")?
            .to_owned();
        let workspace = root.join(ws);
        let package = loop {
            let manifest = directory.join("Cargo.toml");
            if manifest.is_file() {
                let value: toml::Value = toml::from_str(&std::fs::read_to_string(manifest)?)?;
                if let Some(name) = value
                    .get("package")
                    .and_then(|v| v.get("name"))
                    .and_then(toml::Value::as_str)
                {
                    break name.to_owned();
                }
            }
            require(
                directory != workspace && directory.starts_with(&workspace),
                format!("changed path has no package: {path}"),
            )?;
            directory.pop();
        };
        selected
            .entry((ws.into(), package))
            .or_default()
            .push(path.clone());
    }
    for ws in [OVERLAY, NATIVE] {
        if selected.contains_key(&(ws.into(), "@workspace".into())) {
            let keys: Vec<_> = selected.keys().filter(|(w, _)| w == ws).cloned().collect();
            let mut all = vec![];
            for key in keys {
                all.extend(selected.remove(&key).unwrap());
            }
            all.sort();
            selected.insert((ws.into(), "@workspace".into()), all);
        }
    }
    Ok(selected)
}
fn source_checkout(args: &Args, root: &Path, lock: &Value) -> Result<PathBuf> {
    let source = args
        .values
        .get("--source")
        .cloned()
        .or_else(|| std::env::var("AI_PLATFORM_AGENT_RUNTIME_SOURCE").ok());
    let fetch = args
        .values
        .get("--fetch-public-source")
        .cloned()
        .or_else(|| std::env::var("AI_PLATFORM_RUST_GATE_FETCH_PUBLIC_SOURCE").ok());
    require(
        source.is_none() || fetch.is_none(),
        "choose controlled checkout or public fetch",
    )?;
    let sha = lock["source"]["upstream_sha"]
        .as_str()
        .ok_or("upstream SHA missing")?;
    require(
        sha.len() == 40
            && sha
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "invalid upstream SHA",
    )?;
    let checkout = if let Some(path) = fetch {
        require(
            lock["source"]["upstream_url"] == "https://github.com/openai/codex.git",
            "only canonical public upstream allowed",
        )?;
        let path = PathBuf::from(path);
        require(!path.exists(), "public fetch destination exists")?;
        std::fs::create_dir_all(&path)?;
        for params in [
            vec!["init", "--quiet"],
            vec![
                "-c",
                "credential.helper=",
                "-c",
                "protocol.file.allow=never",
                "-c",
                "protocol.ext.allow=never",
                "fetch",
                "--no-tags",
                "--depth=1",
                "https://github.com/openai/codex.git",
                sha,
            ],
        ] {
            let status = Command::new("git")
                .args(["--no-replace-objects", "-C"])
                .arg(&path)
                .args(params)
                .env("GIT_ASKPASS", "/bin/false")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .status()?;
            require(status.success(), "public pinned fetch failed")?;
        }
        path
    } else {
        PathBuf::from(source.ok_or("controlled Runtime source required")?)
    };
    require(
        checkout.is_dir() && !checkout.is_symlink(),
        "controlled source must be real directory",
    )?;
    require(
        git(
            &checkout,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )?
        .trim()
        .is_empty(),
        "controlled source must be clean",
    )?;
    require(
        git(&checkout, &["rev-parse", &format!("{sha}^{{commit}}")])?.trim() == sha,
        "upstream commit mismatch",
    )?;
    let _ = root;
    Ok(checkout)
}
fn compose(root: &Path, checkout: &Path, lock: &Value, destination: &Path) -> Result<PathBuf> {
    let sha = lock["source"]["upstream_sha"]
        .as_str()
        .ok_or("missing upstream")?;
    let manifest = object(&root.join("rust/agent-runtime-overlay/manifest.json"))?;
    let identity = source::bundle_identity(&root.join("rust/agent-runtime-overlay"), true)?;
    require(
        identity["sha256"] == manifest["sha256"]
            && identity["file_count"] == manifest["file_count"]
            && manifest["upstream_sha"] == sha,
        "current overlay identity drift",
    )?;
    require(
        lock["build"]["overlay_sha256"] == identity["sha256"]
            && lock["build"]["overlay_file_count"] == identity["file_count"],
        "lock overlay identity drift",
    )?;
    let cargo_sha = digest(&root.join(OVERLAY).join("Cargo.lock"))?;
    require(
        manifest["cargo_lock_sha256"] == cargo_sha
            && lock["build"]["overlay_cargo_lock_sha256"] == cargo_sha,
        "overlay Cargo.lock drift",
    )?;
    let output = git(
        checkout,
        &["ls-tree", "-rz", "--full-tree", sha, "--", "codex-rs"],
    )?;
    let mut entries = BTreeMap::new();
    for record in output.split('\0').filter(|r| !r.is_empty()) {
        let (metadata, path) = record.split_once('\t').ok_or("malformed upstream tree")?;
        let fields: Vec<_> = metadata.split_whitespace().collect();
        require(
            fields.len() == 3
                && fields[1] == "blob"
                && ["100644", "100755", "120000"].contains(&fields[0]),
            "unsupported upstream object",
        )?;
        let relative = path
            .strip_prefix("codex-rs/")
            .ok_or("upstream path outside workspace")?;
        entries.insert(
            relative.to_owned(),
            (fields[0].to_owned(), fields[2].to_owned()),
        );
    }
    let payload = walk(&root.join(OVERLAY), &[], true)?;
    let declared = manifest["upstream_blobs"]
        .as_object()
        .ok_or("v2 overlay upstream blob proof required")?;
    let names: BTreeSet<_> = payload
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    require(
        declared.keys().cloned().collect::<BTreeSet<_>>() == names,
        "overlay preimage inventory mismatch",
    )?;
    for relative in &names {
        safe(&root.join(OVERLAY), relative)?;
        require(
            declared[relative]
                == entries
                    .get(relative)
                    .map(|(_, blob)| json!(blob))
                    .unwrap_or(Value::Null),
            "overlay upstream preimage mismatch",
        )?;
    }
    std::fs::create_dir(destination)?;
    let mut links = vec![];
    for (relative, (mode, blob)) in &entries {
        let path = safe(destination, relative)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let output = Command::new("git")
            .args(["--no-replace-objects", "-C"])
            .arg(checkout)
            .args(["cat-file", "blob", blob])
            .output()?;
        require(output.status.success(), "upstream blob read failed")?;
        if mode == "120000" {
            let target = String::from_utf8(output.stdout)?;
            require(
                !Path::new(&target).is_absolute(),
                "absolute upstream symlink",
            )?;
            let normalized = resolve_path(
                path.parent()
                    .unwrap()
                    .join(&target)
                    .to_str()
                    .ok_or("non UTF8 symlink")?,
            )?;
            require(
                normalized.starts_with(destination.canonicalize()?),
                "escaping upstream symlink",
            )?;
            links.push((path, target));
        } else {
            std::fs::write(&path, output.stdout)?;
            std::fs::set_permissions(
                path,
                std::fs::Permissions::from_mode(if mode == "100755" { 0o755 } else { 0o644 }),
            )?;
        }
    }
    for (path, target) in links {
        std::os::unix::fs::symlink(target, path)?;
    }
    for relative in names {
        let src = safe(&root.join(OVERLAY), &relative)?;
        let dst = safe(destination, &relative)?;
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst)?;
    }
    require(
        digest(&destination.join("Cargo.lock"))? == cargo_sha,
        "composed lock mismatch",
    )?;
    Ok(destination.join("Cargo.toml"))
}
pub fn run(args: &Args) -> Result<Value> {
    let root = args.root().canonicalize()?;
    let evidence = args
        .values
        .get("--evidence-out")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("tmp/gate-evidence/rust-changed-crate-gate.json"));
    if args.flag("--selftest") {
        let p = plan(&root, &[format!("{NATIVE}/Cargo.lock")])?;
        require(
            p.contains_key(&(NATIVE.into(), "@workspace".into())),
            "workspace selection selftest failed",
        )?;
        return Ok(json!({"result":"pass","selftest":"rust-changed-crate-gate"}));
    }
    let base = args.get("--base")?;
    let mut value = json!({"schema_version":"ai-platform/rust-changed-crate-gate/v2","gate":"rust-changed-crate-gate","tier":"L1","base":base,"executor_policy":"hosted-ci-only","crates":{}});
    let execution = (|| -> Result<()> {
        require(
            hosted(),
            "host Rust execution prohibited: hosted CI required",
        )?;
        let changed = selection::changed(&root, base)?;
        let selected = plan(&root, &changed)?;
        value["changed_paths"] = json!(changed);
        if selected.is_empty() {
            value["result"] = json!("not-applicable");
            return Ok(());
        }
        let lock_path = args
            .values
            .get("--lock")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("deploy/agent-runtime-source/lock.json"));
        let lock = object(&lock_path)?;
        let cargo = args
            .values
            .get("--cargo")
            .map(String::as_str)
            .unwrap_or("cargo");
        let rustc = args
            .values
            .get("--rustc")
            .map(String::as_str)
            .unwrap_or("rustc");
        let rustup = args
            .values
            .get("--rustup")
            .map(String::as_str)
            .unwrap_or("rustup");
        let mut toolchains = BTreeMap::<String, String>::new();
        value["toolchains"] = json!({});
        for workspace in selected
            .keys()
            .map(|(workspace, _)| workspace)
            .collect::<BTreeSet<_>>()
        {
            let version = if workspace == OVERLAY {
                lock["build"]["rustc"]
                    .as_str()
                    .and_then(|s| s.split_whitespace().nth(1))
                    .ok_or("locked overlay rustc missing")?
                    .to_owned()
            } else {
                let pin: toml::Value =
                    std::fs::read_to_string(root.join(NATIVE).join("rust-toolchain.toml"))?
                        .parse()?;
                pin["toolchain"]["channel"]
                    .as_str()
                    .ok_or("native pinned toolchain missing")?
                    .to_owned()
            };
            require(
                regex::Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+$")?.is_match(&version),
                "workspace toolchain must pin an exact numeric release",
            )?;
            let mut observed = json!({"channel":version,"invocation":"rustup run","cargo_build_jobs":"1","cargo_incremental":"0"});
            for (tool, exe) in [("cargo", cargo), ("rustc", rustc)] {
                let out = Command::new("timeout")
                    .args(["30", rustup, "run", &version, exe, "--version"])
                    .output()?;
                require(out.status.success(), "workspace toolchain unavailable")?;
                let actual = String::from_utf8(out.stdout)?.trim().to_owned();
                require(
                    if workspace == OVERLAY {
                        lock["build"][tool] == actual
                    } else {
                        actual.starts_with(&format!("{tool} {version} "))
                    },
                    format!("{workspace} {tool} toolchain drift"),
                )?;
                observed[tool] = json!(actual);
            }
            value["toolchains"][workspace] = observed;
            toolchains.insert(workspace.clone(), version);
        }
        value["authority"] = json!({"lock_sha256":digest(&lock_path)?,"upstream_sha":lock["source"]["upstream_sha"]});
        let composed = root
            .join("tmp")
            .join(format!("rust-gate-composed-{}", std::process::id()));
        let mut overlay_manifest = None;
        if selected.keys().any(|(ws, _)| ws == OVERLAY) {
            let checkout = source_checkout(args, &root, &lock)?;
            std::fs::create_dir_all(composed.parent().unwrap())?;
            overlay_manifest = Some(compose(&root, &checkout, &lock, &composed)?);
        }
        let mut passed = true;
        for ((workspace, package), paths) in selected {
            let manifest = if workspace == OVERLAY {
                overlay_manifest.clone().unwrap()
            } else {
                root.join(&workspace).join("Cargo.toml")
            };
            let mut command = vec!["test".to_owned(), "--locked".into()];
            if package == "@workspace" {
                command.push("--workspace".into());
            } else {
                command.extend(["-p".into(), package.clone()]);
            }
            command.extend([
                "--manifest-path".into(),
                manifest.to_string_lossy().into_owned(),
            ]);
            let version = toolchains
                .get(&workspace)
                .ok_or("workspace toolchain missing")?;
            let status = Command::new("timeout")
                .args(["--kill-after=10s", "3300s", rustup, "run", version, cargo])
                .args(&command)
                .current_dir(manifest.parent().unwrap())
                .env("CARGO_BUILD_JOBS", "1")
                .env("CARGO_INCREMENTAL", "0")
                .status()?;
            passed &= status.success();
            value["crates"][format!("{workspace}:{package}")] = json!({"paths":paths,"command":command,"toolchain":version,"status":if status.code()==Some(124){"timeout"}else if status.success(){"pass"}else{"fail"},"return_code":status.code(),"cargo_build_jobs":"1"});
        }
        value["result"] = json!(if passed { "pass" } else { "fail" });
        if composed.exists() {
            std::fs::remove_dir_all(composed)?;
        }
        Ok(())
    })();
    if let Err(error) = execution {
        value["result"] = json!("error");
        value["error"] = json!(error.to_string());
    }
    write_json(&evidence, &value)?;
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workspace_lock_forces_all() {
        let plan = plan(Path::new("."), &[format!("{NATIVE}/Cargo.lock")]).unwrap();
        assert_eq!(plan.len(), 1);
        assert!(plan.contains_key(&(NATIVE.into(), "@workspace".into())));
    }
    #[test]
    fn unmapped_rust_fails_closed() {
        assert!(plan(Path::new("."), &["rust/unmapped/lib.rs".into()]).is_err());
    }
}
