use crate::policy;
use std::{
    path::Path,
    process::{Command, ExitStatus},
};
fn output(args: &[&str]) -> Result<String, String> {
    let result = Command::new("docker")
        .args(args)
        .output()
        .map_err(|e| format!("Docker CLI: {e}"))?;
    if !result.status.success() {
        return Err(format!(
            "Docker command failed ({}): {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    String::from_utf8(result.stdout)
        .map(|s| s.trim().to_owned())
        .map_err(|e| e.to_string())
}
pub fn require_image(image: &str) -> Result<(), String> {
    output(&["image","inspect",image,"--format","{{.Id}}"])
        .map(|_|()).map_err(|_|format!("required local image {image} absent; no pull attempted. Have the primary explicitly select/provision the compiler image first"))
}
pub fn version_label() -> Result<(), String> {
    let version = output(&[
        "image",
        "inspect",
        policy::IMAGE,
        "--format",
        "{{ index .Config.Labels \"agentforge.native.dev.toolchain\" }}",
    ])?;
    if version != "1.99.0" {
        return Err(
            "development image toolchain ownership label mismatch; run explicit prepare".into(),
        );
    }
    Ok(())
}
pub fn owners(root: &Path) -> Result<(), String> {
    output(&["info", "--format", "{{.ServerVersion}}"])?;
    let containers = output(&[
        "ps",
        "--all",
        "--filter",
        &format!("label=com.docker.compose.project={}", policy::PROJECT),
        "--format",
        "{{.ID}}",
    ])?;
    let expected = root.to_str().ok_or("repository path must be UTF8")?;
    for id in containers.lines().filter(|s| !s.is_empty()) {
        let owner = output(&[
            "inspect",
            id,
            "--format",
            "{{ index .Config.Labels \"com.docker.compose.project.working_dir\" }}",
        ])?;
        if owner != expected {
            return Err("existing project contains containers owned by another checkout; no mutation performed".into());
        }
    }
    // An absent volume is created only by an explicit run; existing data is never adopted blindly.
    let exists = Command::new("docker")
        .args(["volume", "inspect", policy::VOLUME])
        .output()
        .map_err(|e| e.to_string())?;
    if exists.status.success() {
        let owner = output(&[
            "volume",
            "inspect",
            policy::VOLUME,
            "--format",
            "{{ index .Labels \"agentforge.native.owner\" }}",
        ])?;
        if owner != expected {
            return Err(
                "native cache volume belongs to another checkout or has no owner; refusing reuse"
                    .into(),
            );
        }
    } else {
        // Distinguish a missing named volume from daemon failure using the already-live daemon.
        output(&["info", "--format", "{{.ServerVersion}}"])?;
    }
    Ok(())
}
pub fn execute(
    root: &Path,
    cargo: &[String],
    fetch: bool,
    binary: Option<&(String, bool, Vec<String>)>,
    public_registry: Option<&Path>,
) -> Result<ExitStatus, String> {
    let mut command = Command::new("docker");
    command.env_clear();
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
            command.env(key, value);
        }
    }
    command
        .env("NATIVE_CONTAINER_ROOT", root)
        .env("COMPOSE_DISABLE_ENV_FILE", "1")
        .env("COMPOSE_PARALLEL_LIMIT", "1");
    command
        .args([
            "compose",
            "--project-name",
            policy::PROJECT,
            "--project-directory",
        ])
        .arg(root)
        .args(["--env-file", "/dev/null", "--file"])
        .arg(root.join("deploy/platform-native/dev/compose.yml"));
    if let Some(path) = public_registry {
        command
            .env("NATIVE_PUBLIC_REGISTRY_SOURCE", path)
            .arg("--file")
            .arg(root.join("deploy/platform-native/dev/compose.registry.yml"));
    }
    if fetch {
        command
            .arg("--file")
            .arg(root.join("deploy/platform-native/dev/compose.fetch.yml"));
    }
    command.args([
        "--profile",
        "native-dev",
        "run",
        "--rm",
        "--no-deps",
        "--pull",
        "never",
        "--name",
        "agentforge-dev-7801d4a9a4-native-build",
        "--entrypoint",
        "/bin/sh",
        "-T",
        "native-build",
        "-ec",
    ]);
    if binary.is_some() {
        command.arg("test \"$(rustc --version | cut -d' ' -f2)\" = 1.99.0; test -x \"$1\" || { echo 'native binary not built; use native-container build --bin first' >&2; exit 2; }; exec \"$@\"").arg("native-binary").args(cargo);
    } else if fetch {
        command.arg("test \"$(rustc --version | cut -d' ' -f2)\" = 1.99.0; target=$(rustc -vV | sed -n 's/^host: //p'); test -n \"$target\"; printf 'public registry fetch compiler target=%s\\n' \"$target\"; exec cargo \"$@\" --target \"$target\"").arg("native-cargo").args(cargo);
    } else {
        command
            .arg("test \"$(rustc --version | cut -d' ' -f2)\" = 1.99.0; exec cargo \"$@\"")
            .arg("native-cargo")
            .args(cargo);
    }
    command
        .status()
        .map_err(|e| format!("Docker execution failed: {e}"))
}
