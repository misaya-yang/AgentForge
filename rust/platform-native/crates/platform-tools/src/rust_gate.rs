//! Hosted-only, lock-pinned changed-crate execution for native and composed workspaces.
use crate::*;
use std::{
    collections::BTreeSet,
    os::unix::fs::PermissionsExt,
    process::Command,
    time::{Duration, Instant},
};
const OVERLAY: &str = "rust/agent-runtime-overlay/kernel-rs";
const NATIVE: &str = "rust/platform-native";
fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("timeout")
        .args([
            "--kill-after=10s",
            "30s",
            "git",
            "--no-replace-objects",
            "-C",
        ])
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
fn remaining(deadline: Instant) -> Result<String> {
    let seconds = deadline.saturating_duration_since(Instant::now()).as_secs();
    require(seconds > 0, "Rust gate total 55-minute budget exhausted")?;
    Ok(format!("{seconds}s"))
}
fn source_checkout(args: &Args, root: &Path, lock: &Value, deadline: Instant) -> Result<PathBuf> {
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
            let status = Command::new("timeout")
                .args([
                    "--kill-after=10s",
                    &remaining(deadline)?,
                    "git",
                    "--no-replace-objects",
                    "-C",
                ])
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
fn compose(
    root: &Path,
    checkout: &Path,
    lock: &Value,
    destination: &Path,
    deadline: Instant,
) -> Result<PathBuf> {
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
        remaining(deadline)?;
        let path = safe(destination, relative)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let output = Command::new("timeout")
            .args([
                "--kill-after=10s",
                &remaining(deadline)?,
                "git",
                "--no-replace-objects",
                "-C",
            ])
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
        remaining(deadline)?;
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
fn cargo_selection(package: &str, manifest: &Path) -> Vec<String> {
    let mut args = vec!["--locked".into()];
    if package == "@workspace" {
        args.push("--workspace".into());
    } else {
        args.extend(["-p".into(), package.into()]);
    }
    args.extend([
        "--manifest-path".into(),
        manifest.to_string_lossy().into_owned(),
    ]);
    args
}
fn commands(
    package: &str,
    manifest: &Path,
    threads: usize,
    doctests: bool,
) -> Vec<(String, Vec<String>)> {
    let selection = cargo_selection(package, manifest);
    let mut nextest = vec![
        "nextest".into(),
        "run".into(),
        "--no-fail-fast".into(),
        "--ignore-default-filter".into(),
        "--no-tests=fail".into(),
        "--test-threads".into(),
        threads.to_string(),
    ];
    nextest.extend(selection.clone());
    let mut phases = vec![("nextest".into(), nextest)];
    if doctests {
        let mut docs = vec!["test".into(), "--doc".into()];
        docs.extend(selection);
        phases.push(("doctests".into(), docs));
    }
    phases
}
fn has_doctests(metadata: &Value, package: &str) -> Result<bool> {
    let members = metadata["workspace_members"]
        .as_array()
        .ok_or("Cargo workspace members missing")?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or("Cargo metadata packages missing")?;
    let selected: Vec<_> = packages
        .iter()
        .filter(|p| members.contains(&p["id"]) && (package == "@workspace" || p["name"] == package))
        .collect();
    require(
        !selected.is_empty(),
        "selected Cargo package missing from metadata",
    )?;
    Ok(selected.iter().any(|p| {
        p["targets"].as_array().is_some_and(|targets| {
            targets.iter().any(|t| {
                t["doctest"] == true
                    && t["kind"].as_array().is_some_and(|kinds| {
                        kinds.iter().any(|k| {
                            ["lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"]
                                .contains(&k.as_str().unwrap_or(""))
                        })
                    })
            })
        })
    }))
}
fn no_tests_fallback(code: Option<i32>) -> bool {
    // nextest's documented NoTestsRun code, never a build/test/tool/timeout error.
    code == Some(4)
}
fn execute_tests(
    package: &str,
    manifest: &Path,
    threads: usize,
    doctests: bool,
    key: &str,
    value: &mut Value,
    evidence: &Path,
    mut execute: impl FnMut(&[String]) -> Result<std::process::ExitStatus>,
) -> Result<bool> {
    let mut phases = commands(package, manifest, threads, doctests);
    let mut package_passed = true;
    let mut fallback = false;
    for (phase, command) in &mut phases {
        if fallback && phase == "doctests" {
            // The exact original Cargo invocation below already includes doctests.
            continue;
        }
        value["stage"] = json!(format!("{phase}:{key}"));
        write_json(evidence, value)?;
        let phase_started = Instant::now();
        let status = execute(command)?;
        let no_tests = phase == "nextest" && no_tests_fallback(status.code());
        value["crates"][key]["phases"].as_array_mut().unwrap().push(json!({"phase":phase,"command":command,"status":if no_tests {"no-tests:requires-cargo-fallback"} else if status.code()==Some(124) {"timeout"} else if status.success(){"pass"}else{"fail"},"return_code":status.code(),"elapsed_seconds":phase_started.elapsed().as_secs_f64()}));
        if no_tests {
            fallback = true;
            let mut original = vec!["test".to_owned()];
            original.extend(cargo_selection(package, manifest));
            value["stage"] = json!(format!("cargo-no-tests-fallback:{key}"));
            write_json(evidence, value)?;
            let fallback_started = Instant::now();
            let fallback_status = execute(&original)?;
            value["crates"][key]["phases"].as_array_mut().unwrap().push(json!({"phase":"cargo-no-tests-fallback","command":original,"status":if fallback_status.success(){"pass"}else{"fail"},"return_code":fallback_status.code(),"elapsed_seconds":fallback_started.elapsed().as_secs_f64()}));
            package_passed &= fallback_status.success();
            value["crates"][key]["doctests"] = json!("included-in-original-cargo-fallback");
        } else {
            package_passed &= status.success();
            if phase == "doctests" {
                value["crates"][key]["doctests"] =
                    json!(if status.success() { "pass" } else { "fail" });
            }
        }
        write_json(evidence, value)?;
    }
    Ok(package_passed)
}
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        if self.0.exists() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
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
    let started = Instant::now();
    let deadline = started + Duration::from_secs(3300);
    let mut value = json!({"schema_version":"ai-platform/rust-changed-crate-gate/v3","gate":"rust-changed-crate-gate","tier":"L1","base":base,"executor_policy":"hosted-ci-only","budget_seconds":3300,"result":"in-progress","stage":"preflight","crates":{}});
    write_json(&evidence, &value)?;
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
        let capacity = ci_capacity_policy::detect()?;
        value["capacity"] = json!({"hosted":capacity.hosted,"cpus":capacity.cpus,"memory_mib":capacity.memory_mib,"cargo_build_jobs":capacity.build_jobs,"nextest_test_threads":capacity.test_threads});
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
            let mut observed = json!({"channel":version,"invocation":"rustup run","cargo_build_jobs":capacity.build_jobs,"cargo_incremental":"0"});
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
            let nextest = Command::new("timeout")
                .args(["30", rustup, "run", &version, cargo, "nextest", "--version"])
                .output()?;
            let nextest_version = String::from_utf8(nextest.stdout)?;
            require(
                nextest.status.success() && nextest_version.starts_with("cargo-nextest 0.9.148 "),
                "pinned cargo-nextest 0.9.148 required",
            )?;
            observed["nextest"] = json!(nextest_version.trim());
            value["toolchains"][workspace] = observed;
            toolchains.insert(workspace.clone(), version);
        }
        value["authority"] = json!({"lock_sha256":digest(&lock_path)?,"upstream_sha":lock["source"]["upstream_sha"]});
        let composed = Scratch(
            std::env::temp_dir().join(format!("rust-gate-composed-{}", uuid::Uuid::new_v4())),
        );
        require(
            !resolve_path(composed.0.to_str().ok_or("scratch path must be UTF8")?)?
                .starts_with(&root),
            "Rust gate scratch must be external",
        )?;
        let mut overlay_manifest = None;
        if selected.keys().any(|(ws, _)| ws == OVERLAY) {
            value["stage"] = json!("source-assembly");
            write_json(&evidence, &value)?;
            let checkout = source_checkout(args, &root, &lock, deadline)?;
            overlay_manifest = Some(compose(&root, &checkout, &lock, &composed.0, deadline)?);
        }
        let mut passed = true;
        for ((workspace, package), paths) in selected {
            let manifest = if workspace == OVERLAY {
                overlay_manifest.clone().unwrap()
            } else {
                root.join(&workspace).join("Cargo.toml")
            };
            let version = toolchains
                .get(&workspace)
                .ok_or("workspace toolchain missing")?;
            let base_command = || -> Result<Command> {
                let mut command = Command::new("timeout");
                command
                    .args([
                        "--kill-after=10s",
                        &remaining(deadline)?,
                        rustup,
                        "run",
                        version,
                        cargo,
                    ])
                    .current_dir(manifest.parent().unwrap())
                    .env("CARGO_BUILD_JOBS", capacity.build_jobs.to_string())
                    .env("CARGO_INCREMENTAL", "0")
                    .env("CARGO_PROFILE_DEV_DEBUG", "0")
                    .env("CARGO_PROFILE_TEST_DEBUG", "0");
                Ok(command)
            };
            let key = format!("{workspace}:{package}");
            value["stage"] = json!(format!("metadata:{key}"));
            write_json(&evidence, &value)?;
            let metadata = base_command()?
                .args([
                    "metadata",
                    "--locked",
                    "--no-deps",
                    "--format-version",
                    "1",
                    "--manifest-path",
                ])
                .arg(&manifest)
                .output()?;
            require(
                metadata.status.success(),
                "Cargo metadata failed; test populations cannot be determined",
            )?;
            let doctests = has_doctests(&serde_json::from_slice(&metadata.stdout)?, &package)?;
            value["crates"][&key] = json!({"paths":paths,"toolchain":version,"cargo_build_jobs":capacity.build_jobs,"phases":[],"doctests":if doctests {"pending"} else {"not-applicable:no-enabled-library-doctests"},"status":"in-progress"});
            let package_passed = execute_tests(
                &package,
                &manifest,
                capacity.test_threads,
                doctests,
                &key,
                &mut value,
                &evidence,
                |command| Ok(base_command()?.args(command).status()?),
            )?;
            passed &= package_passed;
            value["crates"][&key]["status"] = json!(if package_passed { "pass" } else { "fail" });
            write_json(&evidence, &value)?;
        }
        value["result"] = json!(if passed { "pass" } else { "fail" });
        Ok(())
    })();
    if let Err(error) = execution {
        value["result"] = json!("error");
        value["error"] = json!(error.to_string());
    }
    value["elapsed_seconds"] = json!(started.elapsed().as_secs_f64());
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
    #[test]
    fn nextest_preserves_the_exact_selection_and_adds_separate_doctests() {
        for package in ["ai-platform-tools", "@workspace"] {
            let manifest = Path::new("/external/Cargo.toml");
            let phases = commands(package, manifest, 2, true);
            assert_eq!(phases.len(), 2);
            assert_eq!(phases[0].0, "nextest");
            assert_eq!(phases[1].0, "doctests");
            for (_, command) in &phases {
                assert!(command.ends_with(&cargo_selection(package, manifest)));
                assert!(
                    !command
                        .iter()
                        .any(|s| s.starts_with("--exclude") || s == "--lib" || s == "--skip")
                );
            }
            assert!(phases[0].1.contains(&"--no-tests=fail".into()));
            assert!(phases[0].1.contains(&"--ignore-default-filter".into()));
            assert!(phases[1].1.starts_with(&["test".into(), "--doc".into()]));
            assert_eq!(commands(package, manifest, 1, false).len(), 1);
        }
    }
    #[test]
    fn only_enabled_library_docs_in_selected_workspace_members_are_applicable() {
        let metadata = json!({"workspace_members":["bin", "lib", "disabled"],"packages":[
            {"id":"bin","name":"bin-package","targets":[{"kind":["bin"],"doctest":false}]},
            {"id":"lib","name":"lib-package","targets":[{"kind":["lib"],"doctest":true}]},
            {"id":"disabled","name":"disabled-package","targets":[{"kind":["lib"],"doctest":false}]},
            {"id":"dependency","name":"external","targets":[{"kind":["lib"],"doctest":true}]}
        ]});
        assert!(has_doctests(&metadata, "@workspace").unwrap());
        assert!(has_doctests(&metadata, "lib-package").unwrap());
        assert!(!has_doctests(&metadata, "bin-package").unwrap());
        assert!(!has_doctests(&metadata, "disabled-package").unwrap());
        assert!(has_doctests(&metadata, "external").is_err());
        assert!(has_doctests(&metadata, "missing").is_err());
        assert!(has_doctests(&json!({}), "@workspace").is_err());
    }
    #[test]
    fn test_build_tool_and_timeout_failures_never_trigger_no_tests_fallback() {
        assert!(no_tests_fallback(Some(4)));
        for code in [
            None,
            Some(0),
            Some(1),
            Some(100),
            Some(101),
            Some(124),
            Some(127),
        ] {
            assert!(!no_tests_fallback(code));
        }
        assert!(remaining(Instant::now() - Duration::from_secs(1)).is_err());
    }
    #[test]
    fn executed_doctest_and_nextest_failures_fail_the_package() {
        use std::os::unix::process::ExitStatusExt;
        for (codes, expected, expected_phases) in [
            (vec![0, 0], true, vec!["nextest", "doctests"]),
            (vec![0, 101], false, vec!["nextest", "doctests"]),
            (vec![100, 0], false, vec!["nextest", "doctests"]),
            (vec![4, 0], true, vec!["nextest", "cargo-no-tests-fallback"]),
            (
                vec![4, 101],
                false,
                vec!["nextest", "cargo-no-tests-fallback"],
            ),
        ] {
            let scratch = Scratch(
                std::env::temp_dir().join(format!("rust-gate-test-{}", uuid::Uuid::new_v4())),
            );
            let evidence = scratch.0.join("evidence.json");
            let mut value = json!({"crates":{"fixture":{"phases":[]}}});
            let mut codes = codes.into_iter();
            let mut observed = vec![];
            let passed = execute_tests(
                "fixture-package",
                Path::new("/external/Cargo.toml"),
                1,
                true,
                "fixture",
                &mut value,
                &evidence,
                |args| {
                    observed.push(args.to_vec());
                    Ok(std::process::ExitStatus::from_raw(
                        codes.next().expect("unexpected extra command") << 8,
                    ))
                },
            )
            .unwrap();
            assert_eq!(passed, expected);
            assert!(codes.next().is_none());
            let phases: Vec<_> = value["crates"]["fixture"]["phases"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["phase"].as_str().unwrap())
                .collect();
            assert_eq!(phases, expected_phases);
            if expected_phases[1] == "cargo-no-tests-fallback" {
                let mut original = vec!["test".into()];
                original.extend(cargo_selection(
                    "fixture-package",
                    Path::new("/external/Cargo.toml"),
                ));
                assert_eq!(observed[1], original);
            }
            assert_eq!(object(&evidence).unwrap(), value);
        }
    }
}
