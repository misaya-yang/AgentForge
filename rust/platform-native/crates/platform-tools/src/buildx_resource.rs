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
    let context = execute(&["context".into(), "show".into()])?;
    if !context.success {
        return Err("Docker context lookup failed".into());
    }
    let context = context.stdout.trim();
    if context.is_empty() || context.lines().count() != 1 {
        return Err("Docker returned an invalid context name".into());
    }
    let context = context.to_owned();
    let name = builder_name(&root, &context, memory, cpu_quota);
    let inspect = vec!["buildx".into(), "inspect".into(), name.clone()];
    let existing = execute(&inspect)?;
    if !existing.success {
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
            context,
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

pub fn run(args: &Args) -> Result<Value> {
    if args.flag("--help")
        || args
            .positionals
            .get(1)
            .is_some_and(|arg| arg == "-h" || arg == "--help")
    {
        return Ok(serde_json::json!({
            "_plain": "usage: platform-tools buildx-resource-builder --repo-root PATH --memory Nm|Ng --cpu-quota N"
        }));
    }
    let memory = args.get("--memory")?;
    let cpu_quota = args
        .get("--cpu-quota")?
        .parse::<u64>()
        .map_err(|_| "CPU quota must be a positive integer")?;
    let root = Path::new(args.get("--repo-root")?);
    let name = ensure_builder_with(root, memory, cpu_quota, execute_docker)?;
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
    fn creates_a_scoped_builder_and_verifies_actual_container_limits() {
        let root = root();
        let expected_name = super::builder_name(
            &root.canonicalize().unwrap(),
            "desktop-linux",
            "4g",
            200_000,
        );
        let calls = RefCell::new(Vec::<Vec<String>>::new());
        let name = ensure_builder_with(&root, "4g", 200_000, |args| {
            calls.borrow_mut().push(args.to_vec());
            match args {
                [context, show] if context == "context" && show == "show" => {
                    Ok(output(true, "desktop-linux\n"))
                }
                [buildx, inspect, _] if buildx == "buildx" && inspect == "inspect" => {
                    let count = calls
                        .borrow()
                        .iter()
                        .filter(|call| call.first().is_some_and(|v| v == "buildx"))
                        .count();
                    if count == 1 {
                        Ok(output(false, ""))
                    } else {
                        Ok(output(true, "Name: test\nDriver: docker-container\n"))
                    }
                }
                [buildx, create, ..] if buildx == "buildx" && create == "create" => {
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
                [container, inspect, format, template, name]
                    if container == "container"
                        && inspect == "inspect"
                        && format == "--format"
                        && template == "{{json .HostConfig}}" =>
                {
                    assert_eq!(name, &format!("buildx_buildkit_{expected_name}0"));
                    Ok(output(true, r#"{"Memory":4294967296,"CpuQuota":200000}"#))
                }
                _ => panic!("unexpected Docker command: {args:?}"),
            }
        })
        .unwrap();
        assert_eq!(name, expected_name);
        let calls = calls.into_inner();
        assert_eq!(calls.len(), 5);
        assert!(
            !calls
                .iter()
                .any(|call| call.iter().any(|arg| arg == "use" || arg == "rm"))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_wrong_driver_and_unenforced_resource_limits() {
        let root = root();
        let mut step = 0;
        let result = ensure_builder_with(&root, "1g", 100_000, |_| {
            step += 1;
            Ok(match step {
                1 => output(true, "default\n"),
                2 => output(true, "Name: scoped\nDriver: docker\n"),
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
                2 => output(true, "Name: scoped\nDriver: docker-container\n"),
                3 => output(true, r#"{"Memory":1073741824,"CpuQuota":0}"#),
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
                2 => {
                    assert_eq!(args[0], "buildx");
                    assert_eq!(args[1], "inspect");
                    output(true, "Name: existing\nDriver:        docker-container\n")
                }
                3 => {
                    assert_eq!(args[0], "container");
                    assert_eq!(args[1], "inspect");
                    output(true, r#"{"Memory":2147483648,"CpuQuota":100000}"#)
                }
                _ => panic!("reused builder should require exactly three Docker calls"),
            })
        })
        .unwrap();
        assert_eq!(step, 3);
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
