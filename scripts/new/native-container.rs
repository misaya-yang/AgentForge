//! std-only operational launcher. No Cargo host target, secret loader or business startup.
#[path = "native-container-run.rs"]
mod binary_run;
#[path = "native-container-docker.rs"]
mod docker;
#[path = "native-container-fence.rs"]
mod fence;
#[path = "native-container-policy.rs"]
mod policy;
#[path = "native-container-registry.rs"]
mod registry_source;
#[path = "native-container-watch.rs"]
mod watch;
use std::{
    fs,
    process::{Command, ExitCode},
    time::{SystemTime, UNIX_EPOCH},
};
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn json(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn run() -> Result<u8, String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("--root") || args.len() < 2 {
        return Err("bootstrap requires --root <repository>".into());
    }
    let root = fs::canonicalize(&args[1]).map_err(|e| e.to_string())?;
    args.drain(..2);
    let (explicit_controller, internal_lock_child) = fence::extract(&mut args)?;
    let controller = match explicit_controller {
        Some(path) => Some(path),
        None => fence::configured()?,
    };
    let original_args = args.clone();
    let public_registry = registry_source::extract(&mut args)?;
    let action = args.first().map(String::as_str).unwrap_or("plan");
    if ![
        "plan",
        "doctor",
        "prepare",
        "check",
        "test",
        "build",
        "run",
        "install",
        "watch",
        "engine-build",
        "engine-fetch",
    ]
    .contains(&action)
    {
        return Err("usage: native-container.sh [plan|doctor|prepare [--fetch]|check|test|build] [bounded Cargo target options]".into());
    }
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    if ["plan", "doctor"].contains(&action) && !rest.is_empty() {
        return Err("plan/doctor take no options".into());
    }
    if action == "watch" {
        if internal_lock_child {
            return Err("internal lock entry is restricted to heavy commands".into());
        }
        return watch::run(
            &root,
            rest,
            public_registry.as_deref(),
            controller.as_deref(),
        );
    }
    if action == "install" {
        binary_run::install(&root, rest)?;
        return Ok(0);
    }
    let run_binary = if action == "run" {
        Some(binary_run::binary(rest)?)
    } else {
        None
    };
    let cargo = if ["engine-build", "engine-fetch"].contains(&action) {
        policy::engine_args(action, rest)?
    } else if ["check", "test", "build"].contains(&action) {
        policy::cargo_args(action, rest)?
    } else {
        vec![]
    };
    if action == "prepare" && !(rest.is_empty() || rest == ["--fetch"]) {
        return Err("prepare accepts only --fetch (explicit public registry network)".into());
    }
    if internal_lock_child && !fence::heavy(action) {
        return Err("internal lock entry is restricted to heavy commands".into());
    }
    if fence::heavy(action) {
        let selected = controller.as_deref().ok_or("heavy native-container commands require --lock-controller ABS_PATH to the retained bootstrap platform-tools binary")?;
        if internal_lock_child {
            fence::verify(&root, selected)?;
        } else {
            return fence::delegate(&root, selected, &original_args);
        }
    }
    println!(
        "project={} role=native-build image={} cache={} rust=1.99.0 jobs=1 debug=0 incremental=0 network=none source=read-only",
        policy::PROJECT,
        policy::IMAGE,
        policy::VOLUME
    );
    if let Some(path) = &public_registry {
        println!(
            "public registry directory-source={} (read-only; no target/credentials mounted)",
            path.display()
        );
    }
    if action == "plan" {
        println!(
            "No Docker execution. prepare builds compiler image only; prepare --fetch additionally fills the single Docker Cargo cache. check/test/build execute Cargo afresh, never BuildKit RUN."
        );
        return Ok(0);
    }
    policy::sources(&root)?;
    docker::owners(&root)?;
    if action == "doctor" {
        docker::require_image(policy::IMAGE)?;
        docker::version_label()?;
        println!(
            "doctor: source/ownership/local image checks passed; no containers started; cache contents and rustc execution unverified"
        );
        return Ok(0);
    }
    let cache = policy::external_cache()?;
    fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    if action == "prepare" {
        docker::require_image(policy::BASE)?; // Never auto-pull a large compiler base.
        let status = Command::new("docker")
            .args(["build", "--pull=false", "--tag", policy::IMAGE, "--file"])
            .arg(root.join("deploy/platform-native/dev/Dockerfile"))
            .arg(root.join("deploy/platform-native/dev"))
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Ok(status.code().unwrap_or(1).clamp(1, 255) as u8);
        }
        if rest.is_empty() {
            return Ok(0);
        }
    }
    docker::require_image(policy::IMAGE)?;
    docker::version_label()?;
    let fetch = action == "prepare" || action == "engine-fetch";
    let mut command = if let Some((name, release, tail)) = &run_binary {
        let mut values = vec![format!(
            "/native-cache/target/{}/{}",
            if *release { "release" } else { "debug" },
            name
        )];
        let root_text = root.to_str().ok_or("non-UTF8 root")?;
        values.extend(tail.iter().map(|argument| {
            if argument == root_text {
                "/source".to_owned()
            } else if let Some(relative) = argument.strip_prefix(&format!("{root_text}/")) {
                format!("/source/{relative}")
            } else {
                argument.clone()
            }
        }));
        values
    } else if action == "prepare" {
        vec!["fetch".into(), "--locked".into()]
    } else {
        cargo
    };
    if public_registry.is_some() && run_binary.is_none() {
        registry_source::configure(&mut command);
    }
    let started = now();
    let status = docker::execute(
        &root,
        &command,
        fetch,
        run_binary.as_ref(),
        public_registry.as_deref(),
    )?;
    let code = status.code().unwrap_or(1).clamp(0, 255) as u8;
    let evidence = format!(
        "{{\"schema\":\"agentforge.native-container/v1\",\"action\":{},\"project\":{},\"process_args\":[{}],\"started_unix\":{},\"finished_unix\":{},\"exit_code\":{},\"execution\":\"ephemeral-container-process\",\"buildkit_test_cache\":false,\"runtime_acceptance\":false}}\n",
        json(action),
        json(policy::PROJECT),
        command
            .iter()
            .map(|s| json(s))
            .collect::<Vec<_>>()
            .join(","),
        started,
        now(),
        code
    );
    let output = cache.join(format!("execution-{}-{}.json", started, std::process::id()));
    fs::write(&output, evidence)
        .map_err(|e| format!("Cargo exit {code}; evidence write failed: {e}"))?;
    println!("execution evidence: {}", output.display());
    Ok(code)
}
fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("native-container: {e}");
            ExitCode::from(2)
        }
    }
}
