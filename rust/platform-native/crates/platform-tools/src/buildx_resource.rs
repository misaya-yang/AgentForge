//! Read-only selection of existing builders with verified resource ceilings.
use crate::{Args, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{path::Path, process::Command};

#[derive(Debug)]
struct CommandOutput {
    success: bool,
    stdout: String,
}

fn python_json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_ascii() && !ch.is_control() => out.push(ch),
            ch if (ch as u32) <= 0xffff => {
                out.push_str(&format!("\\u{:04x}", ch as u32));
            }
            ch => {
                let scalar = ch as u32 - 0x1_0000;
                let high = 0xd800 + (scalar >> 10);
                let low = 0xdc00 + (scalar & 0x3ff);
                out.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
            }
        }
    }
    out.push('"');
    out
}

fn memory_bytes(memory: &str) -> std::result::Result<u64, String> {
    let (amount, multiplier) = if let Some(amount) = memory.strip_suffix('m') {
        (amount, 1024u64.pow(2))
    } else if let Some(amount) = memory.strip_suffix('g') {
        (amount, 1024u64.pow(3))
    } else {
        return Err("build memory must be a positive integer with m or g suffix".into());
    };
    if amount.is_empty()
        || amount.starts_with('0')
        || !amount.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("build memory must be a positive integer with m or g suffix".into());
    }
    amount
        .parse::<u64>()
        .ok()
        .and_then(|amount| amount.checked_mul(multiplier))
        .filter(|bytes| *bytes > 0)
        .ok_or_else(|| "build memory is outside the supported range".into())
}

fn builder_name(root: &Path, context: &str, memory: &str, cpu_quota: u64) -> String {
    // Keep the Python helper's json.dumps([root, context, memory, quota]) identity
    // byte-for-byte compatible so a previously provisioned builder is reused.
    let identity = format!(
        "[{}, {}, {}, {}]",
        python_json_string(&root.to_string_lossy()),
        python_json_string(context),
        python_json_string(memory),
        cpu_quota
    );
    let hash = format!("{:x}", Sha256::digest(identity.as_bytes()));
    format!("agentforge-rust-{}", &hash[..16])
}

fn inspected_driver(output: &str) -> Option<&str> {
    output.lines().find_map(|line| {
        let (field, value) = line.split_once(':')?;
        (field.trim() == "Driver").then_some(value.trim())
    })
}

fn local_context_with(
    execute: &mut impl FnMut(&[String]) -> std::result::Result<CommandOutput, String>,
) -> std::result::Result<String, String> {
    let context = execute(&["context".into(), "show".into()])?;
    let name = context.stdout.trim();
    if !context.success || name.is_empty() || name.lines().count() != 1 {
        return Err("Docker context lookup failed".into());
    }
    let endpoint = execute(&[
        "context".into(),
        "inspect".into(),
        name.into(),
        "--format".into(),
        "{{json .Endpoints.docker.Host}}".into(),
    ])?;
    let host: String = serde_json::from_str(&endpoint.stdout)
        .ok()
        .filter(|_: &String| endpoint.success)
        .ok_or("Docker context endpoint unavailable")?;
    if !host.starts_with("unix:///") {
        return Err("local builds require a local Unix-socket Docker context".into());
    }
    Ok(name.into())
}

fn ensure_builder_with(
    root: &Path,
    memory: &str,
    cpu_quota: u64,
    mut execute: impl FnMut(&[String]) -> std::result::Result<CommandOutput, String>,
) -> std::result::Result<String, String> {
    let memory_limit = memory_bytes(memory)?;
    if cpu_quota == 0 {
        return Err("CPU quota must be positive".into());
    }
    let root = root
        .canonicalize()
        .map_err(|_| "repository root is unavailable")?;
    if !root.is_dir() {
        return Err("repository root must be a directory".into());
    }
    let context = local_context_with(&mut execute)?;
    let name = builder_name(&root, &context, memory, cpu_quota);
    let inspect = vec!["buildx".into(), "inspect".into(), name.clone()];
    let existing = execute(&inspect)?;
    if !existing.success {
        return Err("scoped Buildx builder is absent; creation is disabled; explicitly select an existing local builder".into());
    }
    let inspected = existing;
    if !inspected.success || inspected_driver(&inspected.stdout) != Some("docker-container") {
        return Err("scoped Buildx builder has an unexpected driver".into());
    }
    let endpoints: Vec<_> = inspected
        .stdout
        .lines()
        .filter_map(|line| {
            let (field, value) = line.split_once(':')?;
            (field.trim() == "Endpoint").then_some(value.trim())
        })
        .collect();
    if endpoints != [context.as_str()] {
        return Err(
            "scoped Buildx builder must have one node in the selected local context".into(),
        );
    }

    let container = format!("buildx_buildkit_{name}0");
    let config = execute(&[
        "container".into(),
        "inspect".into(),
        "--format".into(),
        "{{json .HostConfig}}".into(),
        container,
    ])?;
    if !config.success {
        return Err("scoped Buildx container inspection failed".into());
    }
    let config: Value = serde_json::from_str(&config.stdout)
        .map_err(|_| "scoped Buildx container configuration is invalid")?;
    if config["Memory"].as_u64() != Some(memory_limit)
        || config["CpuQuota"].as_u64() != Some(cpu_quota)
    {
        return Err("scoped Buildx resource ceilings do not match; refusing to build".into());
    }
    Ok(name)
}

fn execute_docker(args: &[String]) -> std::result::Result<CommandOutput, String> {
    let output = Command::new("docker")
        .args(args)
        .output()
        .map_err(|_| "Docker command is unavailable")?;
    let stdout =
        String::from_utf8(output.stdout).map_err(|_| "Docker returned non-UTF-8 output")?;
    Ok(CommandOutput {
        success: output.status.success(),
        stdout,
    })
}

fn existing_local_builder_with(
    name: &str,
    memory: &str,
    cpu_quota: u64,
    mut execute: impl FnMut(&[String]) -> std::result::Result<CommandOutput, String>,
) -> std::result::Result<String, String> {
    let memory_limit = memory_bytes(memory)?;
    if name.is_empty()
        || name.starts_with('-')
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        || cpu_quota == 0
    {
        return Err(
            "existing builder name and resource ceilings must be explicit and valid".into(),
        );
    }
    let context_name = local_context_with(&mut execute)?;
    let inspected = execute(&["buildx".into(), "inspect".into(), name.into()])?;
    let fields = |field: &str| {
        inspected
            .stdout
            .lines()
            .filter_map(|line| {
                let (key, value) = line.split_once(':')?;
                (key.trim() == field).then(|| value.trim().to_owned())
            })
            .collect::<Vec<_>>()
    };
    if !inspected.success
        || inspected_driver(&inspected.stdout) != Some("docker")
        || fields("Name").first().map(String::as_str) != Some(name)
        || fields("Endpoint") != [context_name]
        || fields("Status") != ["running"]
    {
        return Err("existing builder must be a running single-node docker driver in the selected local context".into());
    }
    let info = execute(&[
        "info".into(),
        "--format".into(),
        "{\"memory\":{{json .MemTotal}},\"cpus\":{{json .NCPU}}}".into(),
    ])?;
    let value: Value = serde_json::from_str(&info.stdout)
        .ok()
        .filter(|_: &Value| info.success)
        .ok_or("Docker daemon capacity unavailable")?;
    let memory = value["memory"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or("Docker daemon memory ceiling unavailable")?;
    let cpu = value["cpus"]
        .as_u64()
        .filter(|n| *n > 0)
        .and_then(|n| n.checked_mul(100_000))
        .ok_or("Docker daemon CPU ceiling unavailable")?;
    if memory > memory_limit || cpu > cpu_quota {
        return Err(
            "existing Docker daemon exceeds the explicitly selected shared memory/CPU ceilings"
                .into(),
        );
    }
    Ok(name.into())
}

pub fn run(args: &Args) -> Result<Value> {
    if args.flag("--help")
        || args
            .positionals
            .get(1)
            .is_some_and(|arg| arg == "-h" || arg == "--help")
    {
        return Ok(serde_json::json!({
            "_plain": "usage: platform-tools buildx-resource-builder --repo-root PATH --memory Nm|Ng --cpu-quota N [--existing-builder NAME]; never creates builders. Existing docker-driver mode checks shared daemon ceilings. AI_PLATFORM_RUST_EXISTING_BUILDX_BUILDER supplies the same explicit selection to build wrappers."
        }));
    }
    let memory = args.get("--memory")?;
    let cpu_quota = args
        .get("--cpu-quota")?
        .parse::<u64>()
        .map_err(|_| "CPU quota must be a positive integer")?;
    let root = Path::new(args.get("--repo-root")?);
    let selected = args
        .values
        .get("--existing-builder")
        .cloned()
        .or_else(|| std::env::var("AI_PLATFORM_RUST_EXISTING_BUILDX_BUILDER").ok())
        .filter(|s| !s.is_empty());
    if std::env::var_os("DOCKER_HOST").is_some_and(|v| !v.is_empty()) {
        return Err("unset DOCKER_HOST; builds require the explicit local Docker context".into());
    }
    let name = if let Some(selected) = selected {
        root.canonicalize()?;
        existing_local_builder_with(&selected, memory, cpu_quota, execute_docker)?
    } else {
        ensure_builder_with(root, memory, cpu_quota, execute_docker)?
    };
    Ok(serde_json::json!({"_plain": name}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn output(success: bool, stdout: &str) -> CommandOutput {
        CommandOutput {
            success,
            stdout: stdout.into(),
        }
    }

    fn root() -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("platform-buildx-resource-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn missing_builder_never_creates_or_selects_one() {
        let root = root();
        let calls = RefCell::new(Vec::new());
        let result = ensure_builder_with(&root, "4g", 200_000, |args| {
            calls.borrow_mut().push(args.to_vec());
            match args {
                [context, show] if context == "context" && show == "show" => {
                    Ok(output(true, "desktop-linux\n"))
                }
                [context, inspect, ..] if context == "context" && inspect == "inspect" => {
                    Ok(output(true, "\"unix:///fixture/docker.sock\""))
                }
                [buildx, inspect, _] if buildx == "buildx" && inspect == "inspect" => {
                    Ok(output(false, ""))
                }
                _ => panic!("missing builder must not cause a mutation: {args:?}"),
            }
        });
        assert!(result.unwrap_err().contains("creation is disabled"));
        assert_eq!(calls.into_inner().len(), 3);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn local_responses() -> Vec<CommandOutput> {
        vec![
            output(true, "desktop-linux\n"),
            output(true, "\"unix:///fixture/docker.sock\""),
            output(
                true,
                "Name: desktop-linux\nDriver: docker\nNodes:\nName: desktop-linux\nEndpoint: desktop-linux\nStatus: running\n",
            ),
            output(true, r#"{"memory":4107141120,"cpus":4}"#),
        ]
    }

    #[test]
    fn existing_local_daemon_selection_is_readonly_and_budgeted() {
        let mut responses = local_responses().into_iter();
        let mut calls = vec![];
        let name = existing_local_builder_with("desktop-linux", "4g", 400_000, |args| {
            calls.push(args.to_vec());
            Ok(responses.next().expect("unexpected extra Docker query"))
        })
        .unwrap();
        assert_eq!(name, "desktop-linux");
        assert_eq!(calls.len(), 4);
        assert!(calls.iter().all(|call| {
            !call
                .iter()
                .any(|v| ["create", "use", "rm", "update", "--bootstrap"].contains(&v.as_str()))
        }));
    }

    #[test]
    fn rejects_remote_wrong_context_multi_node_and_unbounded_daemons() {
        for (step, replacement) in [
            (1, output(true, "\"tcp://remote.invalid:2376\"")),
            (2, output(false, "")),
            (
                2,
                output(
                    true,
                    "Name: other\nDriver: docker\nEndpoint: desktop-linux\nStatus: running\n",
                ),
            ),
            (
                2,
                output(
                    true,
                    "Name: desktop-linux\nDriver: docker-container\nEndpoint: desktop-linux\nStatus: running\n",
                ),
            ),
            (
                2,
                output(
                    true,
                    "Name: desktop-linux\nDriver: docker\nEndpoint: remote\nStatus: running\n",
                ),
            ),
            (
                2,
                output(
                    true,
                    "Name: desktop-linux\nDriver: docker\nEndpoint: desktop-linux\nEndpoint: remote\nStatus: running\n",
                ),
            ),
            (
                2,
                output(
                    true,
                    "Name: desktop-linux\nDriver: docker\nEndpoint: desktop-linux\nStatus: stopped\n",
                ),
            ),
            (3, output(true, r#"{"memory":8589934592,"cpus":4}"#)),
            (3, output(true, r#"{"memory":4107141120,"cpus":8}"#)),
            (3, output(true, r#"{"memory":0,"cpus":4}"#)),
            (3, output(true, "not JSON")),
        ] {
            let mut responses = local_responses();
            responses[step] = replacement;
            let mut responses = responses.into_iter();
            assert!(
                existing_local_builder_with("desktop-linux", "4g", 400_000, |_| {
                    Ok(responses.next().expect("selection must not mutate Docker"))
                })
                .is_err()
            );
        }
        assert!(
            existing_local_builder_with("--bootstrap", "4g", 400_000, |_| panic!(
                "invalid name must not run Docker"
            ))
            .is_err()
        );
    }

    #[test]
    fn rejects_wrong_driver_and_unenforced_resource_limits() {
        let root = root();
        let mut step = 0;
        let result = ensure_builder_with(&root, "1g", 100_000, |_| {
            step += 1;
            Ok(match step {
                1 => output(true, "default\n"),
                2 => output(true, "\"unix:///fixture/docker.sock\""),
                3 => output(true, "Name: scoped\nDriver: docker\n"),
                _ => panic!("unexpected Docker call"),
            })
        });
        assert_eq!(
            result.unwrap_err(),
            "scoped Buildx builder has an unexpected driver"
        );

        let mut step = 0;
        let result = ensure_builder_with(&root, "1g", 100_000, |_| {
            step += 1;
            Ok(match step {
                1 => output(true, "default\n"),
                2 => output(true, "\"unix:///fixture/docker.sock\""),
                3 => output(
                    true,
                    "Name: scoped\nDriver: docker-container\nEndpoint: default\n",
                ),
                4 => output(true, r#"{"Memory":1073741824,"CpuQuota":0}"#),
                _ => panic!("unexpected Docker call"),
            })
        });
        assert_eq!(
            result.unwrap_err(),
            "scoped Buildx resource ceilings do not match; refusing to build"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reuses_a_matching_scoped_builder_without_global_selection() {
        let root = root();
        let mut step = 0;
        let name = ensure_builder_with(&root, "2g", 100_000, |args| {
            step += 1;
            Ok(match step {
                1 => output(true, "default\n"),
                2 => output(true, "\"unix:///fixture/docker.sock\""),
                3 => {
                    assert_eq!(args[0], "buildx");
                    assert_eq!(args[1], "inspect");
                    output(
                        true,
                        "Name: existing\nDriver:        docker-container\nEndpoint: default\n",
                    )
                }
                4 => {
                    assert_eq!(args[0], "container");
                    assert_eq!(args[1], "inspect");
                    output(true, r#"{"Memory":2147483648,"CpuQuota":100000}"#)
                }
                _ => panic!("reused builder should require exactly four Docker calls"),
            })
        })
        .unwrap();
        assert_eq!(step, 4);
        assert!(name.starts_with("agentforge-rust-"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validates_positive_limits_before_running_docker() {
        let root = root();
        for (memory, quota) in [("0g", 1), ("4G", 1), ("1g", 0)] {
            let result = ensure_builder_with(&root, memory, quota, |_| {
                panic!("invalid limits must fail before Docker")
            });
            assert!(result.is_err(), "accepted {memory} with quota {quota}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn builder_name_matches_python_json_unicode_and_del_encoding() {
        let root = Path::new("/tmp/café\u{007f}😀");
        assert_eq!(
            python_json_string("café\u{007f}😀"),
            "\"caf\\u00e9\\u007f\\ud83d\\ude00\""
        );
        assert_eq!(
            builder_name(root, "default", "4g", 200_000),
            "agentforge-rust-7d3c3066dfdcf8df"
        );
    }
}
