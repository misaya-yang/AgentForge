//! Checkout-scoped Buildx builders with verified BuildKit resource ceilings.
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

#[derive(Debug, PartialEq, Eq)]
struct BuilderNode {
    name: String,
    endpoint: Option<String>,
}

fn inspected_nodes(output: &str) -> Vec<BuilderNode> {
    let mut in_nodes = false;
    let mut name = None;
    let mut endpoint = None;
    let mut nodes = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line == "Nodes:" {
            in_nodes = true;
            continue;
        }
        if !in_nodes {
            continue;
        }
        let Some((field, value)) = line.split_once(':') else {
            continue;
        };
        match field.trim() {
            "Name" => {
                if let Some(name) = name.replace(value.trim().to_owned()) {
                    nodes.push(BuilderNode { name, endpoint });
                }
                endpoint = None;
            }
            "Endpoint" => endpoint = Some(value.trim().to_owned()),
            _ => {}
        }
    }
    if let Some(name) = name {
        nodes.push(BuilderNode { name, endpoint });
    }
    nodes
}

fn selected_builder(
    command_line: Option<&str>,
    environment: Option<&str>,
) -> std::result::Result<Option<String>, String> {
    let Some(value) = command_line.or(environment) else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
    {
        return Err("Buildx builder name is invalid".into());
    }
    Ok(Some(value.to_owned()))
}

fn current_context_with(
    mut execute: impl FnMut(&[String]) -> std::result::Result<CommandOutput, String>,
) -> std::result::Result<(String, String), String> {
    let context = execute(&["context".into(), "show".into()])?;
    if !context.success {
        return Err("Docker context lookup failed".into());
    }
    let context = context.stdout.trim();
    if context.is_empty() || context.lines().count() != 1 {
        return Err("Docker returned an invalid context name".into());
    }
    let context = context.to_owned();
    let endpoint = execute(&[
        "context".into(),
        "inspect".into(),
        "--format".into(),
        "{{json .Endpoints}}".into(),
        context.clone(),
    ])?;
    if !endpoint.success {
        return Err("Docker context endpoint lookup failed".into());
    }
    let endpoints: Value = serde_json::from_str(&endpoint.stdout)
        .map_err(|_| "Docker returned invalid context endpoints")?;
    let endpoint = endpoints["docker"]["Host"]
        .as_str()
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .ok_or("Docker context has no Docker endpoint")?;
    Ok((context, endpoint.to_owned()))
}

fn ensure_builder_with(
    root: &Path,
    memory: &str,
    cpu_quota: u64,
    requested_builder: Option<&str>,
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
    let (context, context_endpoint) = current_context_with(&mut execute)?;
    let explicit = selected_builder(requested_builder, None)?;
    let name = explicit
        .clone()
        .unwrap_or_else(|| builder_name(&root, &context, memory, cpu_quota));
    let inspect = vec!["buildx".into(), "inspect".into(), name.clone()];
    let existing = execute(&inspect)?;
    if !existing.success {
        if explicit.is_some() {
            return Err("selected Buildx builder does not exist; refusing to create it".into());
        }
        let node = format!("{name}0");
        let create = vec![
            "buildx".into(),
            "create".into(),
            "--name".into(),
            name.clone(),
            "--node".into(),
            node,
            "--driver".into(),
            "docker-container".into(),
            "--driver-opt".into(),
            format!("memory={memory}"),
            "--driver-opt".into(),
            format!("cpu-quota={cpu_quota}"),
            "--bootstrap".into(),
            context.clone(),
        ];
        let created = execute(&create)?;
        if !created.success {
            return Err("scoped Buildx builder creation failed".into());
        }
    }

    let inspected = if existing.success {
        existing
    } else {
        execute(&inspect)?
    };
    if !inspected.success || inspected_driver(&inspected.stdout) != Some("docker-container") {
        return Err("scoped Buildx builder has an unexpected driver".into());
    }
    let nodes = inspected_nodes(&inspected.stdout);
    if nodes.len() != 1 {
        return Err("scoped Buildx builder must have exactly one node".into());
    }
    let node = &nodes[0];
    let endpoint = node
        .endpoint
        .as_deref()
        .filter(|endpoint| !endpoint.is_empty())
        .ok_or("scoped Buildx builder node has no endpoint")?;
    if endpoint != context && endpoint != context_endpoint {
        return Err(
            "scoped Buildx builder endpoint does not match the current Docker context".into(),
        );
    }

    let container = format!("buildx_buildkit_{}", node.name);
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

pub fn run(args: &Args) -> Result<Value> {
    if args.flag("--help")
        || args
            .positionals
            .get(1)
            .is_some_and(|arg| arg == "-h" || arg == "--help")
    {
        return Ok(serde_json::json!({
            "_plain": "usage: platform-tools buildx-resource-builder --repo-root PATH --memory Nm|Ng --cpu-quota N [--builder NAME] (or set BUILDX_BUILDER)"
        }));
    }
    let memory = args.get("--memory")?;
    let cpu_quota = args
        .get("--cpu-quota")?
        .parse::<u64>()
        .map_err(|_| "CPU quota must be a positive integer")?;
    let root = Path::new(args.get("--repo-root")?);
    if args.flag("--builder") && !args.values.contains_key("--builder") {
        return Err("missing --builder".into());
    }
    let requested_builder = selected_builder(
        args.values.get("--builder").map(String::as_str),
        std::env::var("BUILDX_BUILDER").ok().as_deref(),
    )?;
    let name = ensure_builder_with(
        root,
        memory,
        cpu_quota,
        requested_builder.as_deref(),
        execute_docker,
    )?;
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

    fn args_are(args: &[String], expected: &[&str]) -> bool {
        args.iter().map(String::as_str).eq(expected.iter().copied())
    }

    fn context_response(args: &[String], endpoint: &str) -> Option<CommandOutput> {
        if args_are(args, &["context", "show"]) {
            Some(output(true, "desktop-linux\n"))
        } else if args_are(
            args,
            &[
                "context",
                "inspect",
                "--format",
                "{{json .Endpoints}}",
                "desktop-linux",
            ],
        ) {
            Some(output(
                true,
                &format!(r#"{{"docker":{{"Host":"{endpoint}"}}}}"#),
            ))
        } else {
            None
        }
    }

    fn inspect_output(driver: &str, nodes: &[(&str, &str)]) -> String {
        let mut result = format!("Name: selected\nDriver:        {driver}\nNodes:\n");
        for (name, endpoint) in nodes {
            result.push_str(&format!(
                "Name:           {name}\nEndpoint:       {endpoint}\nStatus:         running\n"
            ));
        }
        result
    }

    #[test]
    fn creates_a_scoped_builder_and_verifies_actual_container_limits() {
        let root = root();
        let expected_name = super::builder_name(
            &root.canonicalize().unwrap(),
            "desktop-linux",
            "4g",
            200_000,
        );
        let calls = RefCell::new(Vec::<Vec<String>>::new());
        let name = ensure_builder_with(&root, "4g", 200_000, None, |args| {
            calls.borrow_mut().push(args.to_vec());
            if let Some(response) = context_response(args, "unix:///var/run/docker.sock") {
                return Ok(response);
            }
            match calls.borrow().len() {
                3 if args_are(args, &["buildx", "inspect", &expected_name]) => {
                    Ok(output(false, ""))
                }
                4 if args.first().is_some_and(|arg| arg == "buildx")
                    && args.get(1).is_some_and(|arg| arg == "create") =>
                {
                    assert!(
                        args.windows(2)
                            .any(|w| w[0] == "--driver" && w[1] == "docker-container")
                    );
                    assert!(
                        args.windows(2)
                            .any(|w| w[0] == "--driver-opt" && w[1] == "memory=4g")
                    );
                    assert!(
                        args.windows(2)
                            .any(|w| { w[0] == "--driver-opt" && w[1] == "cpu-quota=200000" })
                    );
                    assert!(args.iter().any(|arg| arg == "desktop-linux"));
                    Ok(output(true, ""))
                }
                5 if args_are(args, &["buildx", "inspect", &expected_name]) => Ok(output(
                    true,
                    &inspect_output(
                        "docker-container",
                        &[(&format!("{expected_name}0"), "unix:///var/run/docker.sock")],
                    ),
                )),
                6 if args_are(
                    args,
                    &[
                        "container",
                        "inspect",
                        "--format",
                        "{{json .HostConfig}}",
                        &format!("buildx_buildkit_{expected_name}0"),
                    ],
                ) =>
                {
                    Ok(output(true, r#"{"Memory":4294967296,"CpuQuota":200000}"#))
                }
                _ => panic!("unexpected Docker command: {args:?}"),
            }
        })
        .unwrap();
        assert_eq!(name, expected_name);
        let calls = calls.into_inner();
        assert_eq!(calls.len(), 6);
        assert!(
            !calls
                .iter()
                .any(|call| call.iter().any(|arg| arg == "use" || arg == "rm"))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reuses_the_explicit_builder_without_creating_or_globally_selecting_it() {
        let root = root();
        let builder = "agentforge-rust-c4b4565e14f050b5";
        let node = format!("{builder}0");
        let mut step = 0;
        let name = ensure_builder_with(&root, "4g", 200_000, Some(builder), |args| {
            step += 1;
            if let Some(response) = context_response(args, "unix:///var/run/docker.sock") {
                return Ok(response);
            }
            Ok(match step {
                3 => {
                    assert!(args_are(args, &["buildx", "inspect", builder]));
                    output(
                        true,
                        &inspect_output("docker-container", &[(&node, "desktop-linux")]),
                    )
                }
                4 => {
                    assert!(args_are(
                        args,
                        &[
                            "container",
                            "inspect",
                            "--format",
                            "{{json .HostConfig}}",
                            &format!("buildx_buildkit_{node}"),
                        ]
                    ));
                    output(true, r#"{"Memory":4294967296,"CpuQuota":200000}"#)
                }
                _ => panic!("explicit builder reuse should not create or select a builder"),
            })
        });
        assert_eq!(name.unwrap(), builder);
        assert_eq!(step, 4);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_explicit_builder_fails_without_implicit_creation() {
        let root = root();
        let mut calls = Vec::new();
        let result = ensure_builder_with(&root, "4g", 200_000, Some("already-managed"), |args| {
            calls.push(args.to_vec());
            if let Some(response) = context_response(args, "unix:///var/run/docker.sock") {
                return Ok(response);
            }
            assert!(args_are(args, &["buildx", "inspect", "already-managed"]));
            Ok(output(false, ""))
        });
        assert_eq!(
            result.unwrap_err(),
            "selected Buildx builder does not exist; refusing to create it"
        );
        assert_eq!(calls.len(), 3);
        assert!(
            !calls
                .iter()
                .any(|call| call.get(1).is_some_and(|arg| arg == "create"))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_driver_context_node_and_resource_mismatches() {
        let root = root();
        let builder = "agentforge-rust-c4b4565e14f050b5";
        let existing_result = |inspection: String, host_config: &'static str| {
            let mut step = 0;
            ensure_builder_with(&root, "4g", 200_000, Some(builder), |args| {
                step += 1;
                if let Some(response) = context_response(args, "unix:///var/run/docker.sock") {
                    return Ok(response);
                }
                Ok(match step {
                    3 => output(true, &inspection),
                    4 => output(true, host_config),
                    _ => panic!("unexpected Docker command"),
                })
            })
        };

        assert_eq!(
            existing_result(
                inspect_output("docker-container", &[("one", "unix:///other/docker.sock")],),
                r#"{"Memory":4294967296,"CpuQuota":200000}"#,
            )
            .unwrap_err(),
            "scoped Buildx builder endpoint does not match the current Docker context"
        );
        assert_eq!(
            existing_result(
                inspect_output(
                    "docker-container",
                    &[
                        ("one", "unix:///var/run/docker.sock"),
                        ("two", "unix:///var/run/docker.sock"),
                    ],
                ),
                r#"{"Memory":4294967296,"CpuQuota":200000}"#,
            )
            .unwrap_err(),
            "scoped Buildx builder must have exactly one node"
        );
        assert_eq!(
            existing_result(
                inspect_output("docker", &[("one", "unix:///var/run/docker.sock")]),
                r#"{"Memory":4294967296,"CpuQuota":200000}"#,
            )
            .unwrap_err(),
            "scoped Buildx builder has an unexpected driver"
        );
        assert_eq!(
            existing_result(
                inspect_output(
                    "docker-container",
                    &[("one", "unix:///var/run/docker.sock")],
                ),
                r#"{"Memory":2147483648,"CpuQuota":200000}"#,
            )
            .unwrap_err(),
            "scoped Buildx resource ceilings do not match; refusing to build"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cli_builder_overrides_environment_and_names_are_validated() {
        assert_eq!(
            selected_builder(Some("cli-builder"), Some("env-builder")).unwrap(),
            Some("cli-builder".into())
        );
        assert_eq!(
            selected_builder(None, Some("agentforge-rust-c4b4565e14f050b5")).unwrap(),
            Some("agentforge-rust-c4b4565e14f050b5".into())
        );
        assert!(selected_builder(None, Some("bad/builder")).is_err());
        assert!(selected_builder(Some("  "), None).is_err());
    }

    #[test]
    fn validates_positive_limits_before_running_docker() {
        let root = root();
        for (memory, quota) in [("0g", 1), ("4G", 1), ("1g", 0)] {
            let result = ensure_builder_with(&root, memory, quota, None, |_| {
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
