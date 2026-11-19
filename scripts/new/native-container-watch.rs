use crate::policy;
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
fn fingerprint(path: &Path, hasher: &mut DefaultHasher) -> Result<(), String> {
    path.hash(hasher);
    if path.is_dir() {
        let mut children = fs::read_dir(path)
            .map_err(|e| e.to_string())?
            .map(|e| e.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        children.sort();
        for child in children {
            fingerprint(&child, hasher)?;
        }
    } else {
        fs::read(path).map_err(|e| e.to_string())?.hash(hasher);
    }
    Ok(())
}
fn snapshot(paths: &[PathBuf]) -> Result<u64, String> {
    let mut hasher = DefaultHasher::new();
    for path in paths {
        if !path.exists() {
            path.hash(&mut hasher);
            "missing".hash(&mut hasher);
            continue;
        }
        policy::scan(path)?;
        fingerprint(path, &mut hasher)?;
    }
    Ok(hasher.finish())
}
#[derive(Debug, PartialEq, Eq)]
struct Invocation {
    program: PathBuf,
    arguments: Vec<String>,
    restart: bool,
}
fn invocation(
    root: &Path,
    tail: &[String],
    restart: bool,
    env: Option<&Path>,
    registry: Option<&Path>,
    controller: Option<&Path>,
) -> Result<Invocation, String> {
    if restart {
        if registry.is_some() {
            return Err("restart uses hot-update's sole image build, not the dev registry source; omit --public-registry-source".into());
        }
        let env = env.ok_or("watch --restart requires --env <repository>/.env.dev")?;
        if env != root.join(".env.dev") {
            return Err("restart accepts only this repository's selected .env.dev".into());
        }
        if tail.is_empty()
            || tail
                .iter()
                .any(|a| !["--gateway", "--knowledge"].contains(&a.as_str()))
        {
            return Err("restart requires explicit --gateway and/or --knowledge after --; Cargo targets/frontend/all are not restart scopes".into());
        }
        let mut arguments = tail.to_vec();
        arguments.extend([
            "--env".into(),
            env.to_str().ok_or("non-UTF8 env path")?.into(),
        ]);
        let controller = controller.ok_or(
            "restart watch requires --lock-controller ABS_PATH or external lock-controller.path",
        )?;
        let mut fenced=vec!["resource-lock".into(),"run".into(),"--root".into(),root.to_string_lossy().into_owned(),"--resource".into(),"integration-runtime".into(),"--expected-end-condition".into(),"Selected hot-update completes or fails; canonical ownership and paired-image checks retained; controller releases integration-runtime/low-memory fences".into(),"--".into(),root.join("scripts/new/hot-update.sh").to_string_lossy().into_owned()];
        fenced.extend(arguments);
        Ok(Invocation {
            program: controller.to_path_buf(),
            arguments: fenced,
            restart: true,
        })
    } else {
        if env.is_some() {
            return Err("runtime --env is forbidden in build-only watch".into());
        }
        policy::cargo_args("build", tail)?;
        let mut arguments = vec![
            "--root".into(),
            root.to_str().ok_or("non-UTF8 root")?.into(),
            "build".into(),
        ];
        if let Some(source) = registry {
            arguments.extend([
                "--public-registry-source".into(),
                source.to_str().ok_or("non-UTF8 registry")?.into(),
            ]);
        }
        let controller =
            controller.ok_or("build-only watch requires --lock-controller ABS_PATH")?;
        arguments.extend([
            "--lock-controller".into(),
            controller.to_string_lossy().into_owned(),
        ]);
        arguments.extend_from_slice(tail);
        Ok(Invocation {
            program: std::env::current_exe().map_err(|e| e.to_string())?,
            arguments,
            restart: false,
        })
    }
}
pub fn run(
    root: &Path,
    args: &[String],
    registry: Option<&Path>,
    controller: Option<&Path>,
) -> Result<u8, String> {
    let restart = args.first().map(String::as_str) == Some("--restart");
    let mut index = if args
        .first()
        .is_some_and(|a| a == "--restart" || a == "--build-only")
    {
        1
    } else {
        0
    };
    let mut env = None;
    let mut paths = vec![];
    while index < args.len() && args[index] != "--" {
        let option = args[index].as_str();
        let value = args.get(index + 1).ok_or("watch option requires value")?;
        if option == "--env" {
            if env.is_some() {
                return Err("duplicate --env".into());
            }
            let selected = root.join(value);
            // Exact selected path only; do not read or export private file contents here.
            if selected != root.join(".env.dev") {
                return Err("watch restart requires the selected repository .env.dev".into());
            }
            env = Some(selected);
        } else if option == "--watch-path" {
            let path = fs::canonicalize(root.join(value)).map_err(|e| e.to_string())?;
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "watch path escapes repository")?
                .to_str()
                .ok_or("non-UTF8 watch path")?;
            if !policy::SOURCES
                .iter()
                .any(|source| relative == *source || relative.starts_with(&format!("{source}/")))
            {
                return Err("watch path must be an explicit public source mount".into());
            }
            paths.push(path);
        } else {
            return Err(format!("unsupported watch option: {option}"));
        }
        index += 2;
    }
    if paths.is_empty() || args.get(index).map(String::as_str) != Some("--") {
        return Err("watch requires selected public --watch-path values and -- before build targets or restart scopes".into());
    }
    let plan = invocation(
        root,
        &args[index + 1..],
        restart,
        env.as_deref(),
        registry,
        controller,
    )?;
    if restart {
        crate::fence::check_controller(root, &plan.program)?;
        if !root.join(".env.dev").is_file() {
            return Err("selected .env.dev does not exist; no service action performed".into());
        }
        let ignored = Command::new("git")
            .current_dir(root)
            .args(["check-ignore", "--quiet", "--", ".env.dev"])
            .status()
            .map_err(|e| e.to_string())?;
        if !ignored.success() {
            return Err("selected .env.dev must remain Git-ignored".into());
        }
        // Existing hot-update remains authoritative for Docker owner and paired-image locks.
        // Check the public native source boundary without mounting/exporting the private env.
        policy::sources(root)?;
    }
    let mut observed = snapshot(&paths)?;
    loop {
        let mut child = Command::new(&plan.program);
        if plan.restart {
            child.env_clear();
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
                    child.env(key, value);
                }
            }
            child
                .env("COMPOSE_PROJECT_NAME", policy::PROJECT)
                .env("COMPOSE_PARALLEL_LIMIT", "1");
        }
        let status = child
            .current_dir(root)
            .args(&plan.arguments)
            .status()
            .map_err(|e| e.to_string())?;
        println!(
            "watch {} exit={}; {}",
            if plan.restart { "hot-update" } else { "build" },
            status.code().unwrap_or(1),
            if plan.restart {
                "sole hot-update build/replacement attempt; no preceding dev Cargo invocation"
            } else {
                "no service restart performed"
            }
        );
        loop {
            std::thread::sleep(Duration::from_millis(750));
            let next = snapshot(&paths)?;
            if next != observed {
                observed = next;
                break;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_routes_only_through_existing_hot_update() {
        let root = Path::new("/repo");
        let env = root.join(".env.dev");
        let result = invocation(
            root,
            &["--gateway".into(), "--knowledge".into()],
            true,
            Some(&env),
            None,
            Some(Path::new("/bootstrap/platform-tools")),
        )
        .unwrap();
        assert_eq!(result.program, Path::new("/bootstrap/platform-tools"));
        assert!(
            result
                .arguments
                .windows(2)
                .any(|v| v == ["--resource", "integration-runtime"])
        );
        assert_eq!(
            result
                .arguments
                .iter()
                .filter(|a| a.as_str() == "/repo/scripts/new/hot-update.sh")
                .count(),
            1
        );
        assert_eq!(
            &result.arguments[result.arguments.len() - 4..],
            ["--gateway", "--knowledge", "--env", "/repo/.env.dev"]
        );
        assert!(
            !result
                .arguments
                .iter()
                .any(|a| ["build", "cargo", "--no-restart"].contains(&a.as_str()))
        );
    }
    #[test]
    fn restart_rejects_other_envs_scopes_registry_and_cargo_targets() {
        let root = Path::new("/repo");
        let env = root.join(".env.dev");
        for scope in ["--all", "--frontend", "-p", "--no-restart"] {
            assert!(invocation(root, &[scope.into()], true, Some(&env), None, None).is_err());
        }
        assert!(invocation(root, &["--gateway".into()], true, None, None, None).is_err());
        assert!(
            invocation(
                root,
                &["--gateway".into()],
                true,
                Some(Path::new("/other/.env.dev")),
                None,
                None
            )
            .is_err()
        );
        assert!(
            invocation(
                root,
                &["--gateway".into()],
                true,
                Some(&env),
                Some(Path::new("/registry")),
                None,
            )
            .is_err()
        );
    }
    #[test]
    fn build_only_stays_on_dev_cargo_without_runtime_configuration() {
        let root = Path::new("/repo");
        let args = ["-p", "ai-platform-primitives", "--lib"].map(str::to_owned);
        let result = invocation(
            root,
            &args,
            false,
            None,
            None,
            Some(Path::new("/bootstrap/platform-tools")),
        )
        .unwrap();
        assert_eq!(
            result.arguments,
            [
                "--root",
                "/repo",
                "build",
                "--lock-controller",
                "/bootstrap/platform-tools",
                "-p",
                "ai-platform-primitives",
                "--lib"
            ]
        );
        assert!(!result.restart);
        assert!(
            invocation(
                root,
                &args,
                false,
                Some(Path::new("/repo/.env.dev")),
                None,
                None
            )
            .is_err()
        );
    }
    #[test]
    fn build_only_requires_selected_sources() {
        assert!(
            run(
                Path::new("/"),
                &["--build-only".into(), "--".into()],
                None,
                None
            )
            .is_err()
        );
    }
}
