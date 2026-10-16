use crate::*;
use std::collections::{BTreeMap, BTreeSet};
fn zone(name: &str) -> Option<&'static str> {
    if matches!(
        name,
        "ai-platform-gateway"
            | "ai-platform-gateway-foundation"
            | "ai-platform-gateway-integrations"
            | "ai-platform-auth"
    ) {
        Some("gateway")
    } else if name.contains("knowledge") {
        Some("knowledge")
    } else if name == "ai-platform-local-node" {
        Some("local-node")
    } else if name == "ai-platform-sdk-agent-worker" || name == "ai-platform-sdk-agent-engine" {
        Some("sdk-worker")
    } else if name == "ai-platform-agent-runtime" {
        Some("runtime")
    } else if name == "ai-platform-capability-worker" {
        Some("capability-worker")
    } else if matches!(name, "ai-platform-client" | "ai-platform-desktop") {
        Some("client")
    } else {
        None
    }
}
fn dependency_target(
    root: &Path,
    manifest: &Path,
    alias: &str,
    spec: &toml::Value,
    composed: Option<&Path>,
) -> Result<String> {
    let mut spec = spec.clone();
    let mut base = root
        .join(manifest)
        .parent()
        .ok_or("manifest parent missing")?
        .to_owned();
    if spec.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
        let mut parent = base.clone();
        loop {
            let candidate = parent.join("Cargo.toml");
            if candidate.is_file() {
                let value: toml::Value = toml::from_str(&std::fs::read_to_string(candidate)?)?;
                if let Some(dependency) = value
                    .get("workspace")
                    .and_then(|v| v.get("dependencies"))
                    .and_then(|v| v.get(alias))
                {
                    spec = dependency.clone();
                    base = parent.clone();
                    break;
                }
            }
            require(
                parent != root && parent.starts_with(root),
                "workspace dependency cannot be resolved",
            )?;
            parent.pop();
        }
    }
    let declared = spec
        .get("package")
        .and_then(toml::Value::as_str)
        .unwrap_or(alias);
    if let Some(path) = spec.get("path").and_then(toml::Value::as_str) {
        let unresolved = base.join(path).join("Cargo.toml");
        let overlay = root.join("rust/agent-runtime-overlay/kernel-rs");
        // Only the verified overlay workspace can use externally assembled source.
        // Resolve from the corresponding composed package directory, including ../ paths.
        let resolved_via_overlay = composed.is_some() && base.starts_with(&overlay);
        let target = if let Some(composed) = composed.filter(|_| resolved_via_overlay) {
            composed.join(base.strip_prefix(&overlay)?).join(path).join("Cargo.toml").canonicalize()
        } else {
            unresolved.canonicalize()
        }.map_err(|error| format!("cannot resolve dependency {alias} from {}: {}: {error}; supply an identity-verified --composed-workspace for partial runtime overlays", manifest.display(), unresolved.display()))?;
        require(
            target.starts_with(root)
                || (resolved_via_overlay && composed.is_some_and(|c| target.starts_with(c))),
            "path dependency escapes repository and verified composed workspace",
        )?;
        let value: toml::Value = toml::from_str(&std::fs::read_to_string(target)?)?;
        let actual = value
            .get("package")
            .and_then(|v| v.get("name"))
            .and_then(toml::Value::as_str)
            .ok_or("path dependency package name missing")?;
        if spec.get("package").is_some() {
            require(
                actual == declared,
                "path dependency package identity mismatch",
            )?;
        }
        Ok(actual.into())
    } else {
        Ok(declared.into())
    }
}
pub fn check(root: &Path, composed: Option<&Path>) -> Result<Value> {
    let canonical = root.canonicalize()?;
    let root = canonical.as_path();
    let composed = composed.map(Path::canonicalize).transpose()?;
    if let Some(composed) = &composed {
        let overlay = root.join("rust/agent-runtime-overlay/kernel-rs");
        for relative in [
            "Cargo.toml",
            "Cargo.lock",
            "ai-platform-capability-contract/Cargo.toml",
            "ai-platform-capability-worker/Cargo.toml",
        ] {
            require(
                digest(&overlay.join(relative))? == digest(&composed.join(relative))?,
                format!("composed workspace source identity mismatch: {relative}"),
            )?;
        }
    }
    let files = walk(root, crate::harness::PRUNED, false)?;
    let mut packages = BTreeMap::new();
    for path in files
        .iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "Cargo.toml"))
    {
        let v: toml::Value = toml::from_str(&std::fs::read_to_string(root.join(path))?)?;
        if let Some(name) = v
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(toml::Value::as_str)
        {
            require(
                packages
                    .insert(name.to_owned(), (path.clone(), v))
                    .is_none(),
                "duplicate native package identity",
            )?;
        }
    }
    let exceptions = object(
        &root.join("rust/platform-native/crates/platform-tools/native-boundary-exceptions.json"),
    )?;
    require(
        exceptions["schema"] == "ai-platform/native-boundary-exceptions/v1",
        "native boundary exception schema invalid",
    )?;
    let exceptions = exceptions["entries"]
        .as_array()
        .ok_or("native exceptions must be a list")?;
    let mut exception_ids = BTreeSet::new();
    for entry in exceptions {
        require(
            entry["from"] == "ai-platform-knowledge-service"
                && entry["to"] == "ai-platform-gateway",
            "unapproved native migration edge",
        )?;
        for key in ["owner", "reason", "introduced"] {
            require(
                entry[key].as_str().is_some_and(|s| !s.trim().is_empty()),
                "native exception requires ownership/date/rationale",
            )?;
        }
        require(
            exception_ids.insert((entry["from"].to_string(), entry["to"].to_string())),
            "duplicate native exception",
        )?;
    }
    let mut violations = vec![];
    let mut edges = vec![];
    let mut allowlisted = vec![];
    for (name, (path, value)) in &packages {
        let Some(from) = zone(name) else {
            continue;
        };
        // Runtime and build dependencies, including target-specific manifests. Dev tests
        // may compose service processes but may not link another service implementation.
        fn dependencies(v: &toml::Value, out: &mut Vec<(String, toml::Value)>) {
            if let Some(o) = v.as_table() {
                for (k, v) in o {
                    if matches!(
                        k.as_str(),
                        "dependencies" | "build-dependencies" | "dev-dependencies"
                    ) {
                        if let Some(deps) = v.as_table() {
                            for (alias, spec) in deps {
                                out.push((alias.clone(), spec.clone()));
                            }
                        }
                    } else if v.is_table() {
                        dependencies(v, out);
                    }
                }
            }
        }
        let mut deps = vec![];
        dependencies(value, &mut deps);
        for (alias, spec) in deps {
            let target = dependency_target(root, path, &alias, &spec, composed.as_deref())?;
            let Some(to) = zone(&target) else {
                continue;
            };
            edges.push(json!({"from":name,"to":target}));
            if from != to {
                if let Some(exception) = exceptions
                    .iter()
                    .find(|e| e["from"] == *name && e["to"] == target)
                {
                    allowlisted.push(exception.clone());
                    continue;
                }
                // Clients depend only on public contract/client crates, not hosted service internals.
                violations.push(json!({"file":path,"importer_zone":from,"target_module":target,"target_zone":to,"rule":format!("{from} must communicate with {to} through its declared transport, not link its service implementation")}));
            }
        }
    }
    for exception in exceptions {
        if !allowlisted.contains(exception) {
            violations.push(
                json!({"rule":"unused native exception must be removed","exception":exception}),
            );
        }
    }
    let receipt = json!({"gate":"architecture-boundary-gate","tier":"L0","result":if violations.is_empty(){"pass"}else{"fail"},"composed_workspace":composed,"composed_identity_prerequisites":if composed.is_some(){4}else{0},"scanned_manifests":packages.len(),"edges":edges,"violations":violations,"allowlisted":allowlisted});
    let _ = write_json(
        &root.join("tmp/gate-evidence/architecture-boundary-gate.json"),
        &receipt,
    );
    Ok(receipt)
}
fn harness_command_fields<'a>(
    value: &'a serde_json::Value,
    path: &str,
    executable: bool,
    out: &mut Vec<(String, &'a str)>,
) {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                let next = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                // Objects describe named commands/steps, including metadata.
                // Only scalar/list command fields propagate executable context;
                // a `commands` mapping must not turn `purpose` into a command.
                let executable = matches!(
                    key.as_str(),
                    "make"
                        | "shell"
                        | "run"
                        | "command"
                        | "commands"
                        | "entrypoint"
                        | "script"
                        | "argv"
                        | "args"
                );
                harness_command_fields(value, &next, executable, out);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, value) in items.iter().enumerate() {
                harness_command_fields(value, &format!("{path}[{index}]"), executable, out);
            }
        }
        serde_json::Value::String(command) if executable => out.push((path.to_owned(), command)),
        _ => {}
    }
}
pub fn python_inventory(root: &Path, zero: bool) -> Result<Value> {
    let files = walk(root, crate::harness::PRUNED, false)?;
    let mut python = vec![];
    let mut callsites = vec![];
    // Command/token boundaries avoid flagging our own `python-inventory` or
    // `--require-zero-python` command names as interpreter invocations.
    let reference = regex::Regex::new(
        r"(?i)(?:(?:^|[\s;&|(/])python(?:3(?:\.[0-9]+)?)?(?:[\s;)&|]|$)|[A-Za-z0-9_./-]+\.py\b)",
    )?;
    for path in files {
        let is_python = path.extension().and_then(|s| s.to_str()).is_some_and(|s| {
            ["py", "pyw", "pyi"]
                .iter()
                .any(|extension| s.eq_ignore_ascii_case(extension))
        });
        if is_python {
            python.push(path.clone());
        }
        let relevant = path == Path::new("Makefile")
            || path == Path::new("harness.yml")
            || path.starts_with(".github")
            || path.starts_with("scripts")
            || path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("Dockerfile"));
        if !relevant || is_python {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(root.join(&path)) {
            if path == Path::new("harness.yml") {
                // Only executable contract fields are interpreter callsites. Prose,
                // trigger globs and forbidden-artifact patterns do not execute.
                let yaml: serde_yaml::Value = serde_yaml::from_str(&text)?;
                let value = serde_json::to_value(yaml)?;
                let mut commands = Vec::new();
                harness_command_fields(&value, "", false, &mut commands);
                for (field, command) in commands {
                    for line in command
                        .lines()
                        .filter(|line| !line.trim_start().starts_with('#'))
                    {
                        if reference.is_match(line) {
                            callsites
                                .push(json!({"file":path,"field":field,"reference":line.trim()}));
                        }
                    }
                }
                continue;
            }
            for (index, line) in text.lines().enumerate() {
                if line.trim_start().starts_with('#') {
                    continue;
                }
                if reference.is_match(line) {
                    callsites.push(json!({"file":path,"line":index+1,"reference":line.trim()}));
                }
            }
        }
    }
    Ok(
        json!({"gate":"zero-python","result":if zero&&(!python.is_empty()||!callsites.is_empty()){"fail"}else{"pass"},"python_files":python,"operational_references":callsites,"required_zero":zero}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn context_mapping() {
        assert_eq!(zone("ai-platform-gateway-foundation"), Some("gateway"));
        assert_eq!(zone("ai-platform-capability-contract"), None);
        assert_eq!(zone("ai-platform-sdk-agent-engine"), Some("sdk-worker"));
    }
}

#[cfg(test)]
mod resolver_tests {
    use super::*;
    #[test]
    fn composed_resolution_is_limited_to_overlay_and_reads_actual_identity() {
        let temp = std::env::temp_dir().join(format!("boundary-{}", uuid::Uuid::new_v4()));
        let root = temp.join("repo");
        let composed = temp.join("composed");
        std::fs::create_dir_all(root.join("rust/agent-runtime-overlay/kernel-rs/owned")).unwrap();
        std::fs::create_dir_all(composed.join("upstream")).unwrap();
        std::fs::create_dir_all(composed.join("owned")).unwrap();
        std::fs::write(
            composed.join("upstream/Cargo.toml"),
            "[package]\nname='ai-platform-gateway'\n",
        )
        .unwrap();
        let spec: toml::Value = toml::from_str("path='../upstream'").unwrap();
        let target = dependency_target(
            &root,
            Path::new("rust/agent-runtime-overlay/kernel-rs/owned/Cargo.toml"),
            "innocent-alias",
            &spec,
            Some(&composed),
        )
        .unwrap();
        assert_eq!(target, "ai-platform-gateway");
        assert!(
            dependency_target(
                &root,
                Path::new("rust/platform-native/owned/Cargo.toml"),
                "innocent-alias",
                &spec,
                Some(&composed)
            )
            .is_err()
        );
        let absolute = toml::Value::Table(toml::map::Map::from_iter([(
            "path".into(),
            toml::Value::String(composed.join("upstream").display().to_string()),
        )]));
        assert!(
            dependency_target(
                &root,
                Path::new("rust/platform-native/owned/Cargo.toml"),
                "innocent-alias",
                &absolute,
                Some(&composed)
            )
            .is_err()
        );
        std::fs::remove_dir_all(temp).unwrap();
    }
    #[test]
    fn mismatched_composed_identity_is_rejected_before_scanning() {
        let temp = std::env::temp_dir().join(format!("boundary-{}", uuid::Uuid::new_v4()));
        let root = temp.join("repo");
        let composed = temp.join("composed");
        std::fs::create_dir_all(root.join("rust/agent-runtime-overlay/kernel-rs")).unwrap();
        std::fs::create_dir_all(&composed).unwrap();
        std::fs::write(
            root.join("rust/agent-runtime-overlay/kernel-rs/Cargo.toml"),
            "original",
        )
        .unwrap();
        std::fs::write(composed.join("Cargo.toml"), "changed").unwrap();
        assert!(
            check(&root, Some(&composed))
                .unwrap_err()
                .to_string()
                .contains("source identity mismatch: Cargo.toml")
        );
        std::fs::remove_dir_all(temp).unwrap();
    }
}
#[cfg(test)]
mod inventory_packaging_tests {
    use super::*;
    #[test]
    fn harness_metadata_is_not_execution_but_interpreter_commands_remain_forbidden() {
        let root =
            std::env::temp_dir().join(format!("inventory-packaging-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("harness.yml"),"gates:\n  native:\n    make: native-gate\n    purpose: Retired Python Agent entrypoints\nroot_forbidden: ['test_*.py', '*_debug.py']\n").unwrap();
        assert_eq!(python_inventory(&root, true).unwrap()["result"], "pass");
        let metadata = json!({"commands":{"runtime_dependency_gate":{
            "make":"runtime-dependency-gate",
            "purpose":"Retired Python Agent entrypoints",
            "steps":[{"purpose":"python metadata only", "run":"platform-tools python-inventory --require-zero"}]
        }},"root_forbidden":["test_*.py"]});
        std::fs::write(
            root.join("harness.yml"),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        assert_eq!(python_inventory(&root, true).unwrap()["result"], "pass");
        for commands in [
            json!({"actual":{"purpose":"metadata", "shell":"python -m worker"}}),
            json!(["python -m worker"]),
            json!({"actual":{"steps":[{"run":"python -m worker"}]}}),
        ] {
            std::fs::write(
                root.join("harness.yml"),
                serde_json::to_vec(&json!({"commands":commands})).unwrap(),
            )
            .unwrap();
            assert_eq!(python_inventory(&root, true).unwrap()["result"], "fail");
        }
        for command in [
            "python3 worker.py",
            "python -m worker",
            "uv run worker.py",
            "bash scripts/check.py",
        ] {
            std::fs::write(
                root.join("harness.yml"),
                serde_json::to_vec(&json!({"gates":{"actual":{"shell":command}}})).unwrap(),
            )
            .unwrap();
            let result = python_inventory(&root, true).unwrap();
            assert_eq!(result["result"], "fail");
            assert_eq!(
                result["operational_references"][0]["field"],
                "gates.actual.shell"
            );
        }
        std::fs::write(
            root.join("harness.yml"),
            "gates: {native: {make: native-gate}}\n",
        )
        .unwrap();
        std::fs::write(root.join("still.PY"), "fixture").unwrap();
        assert_eq!(python_inventory(&root, true).unwrap()["result"], "fail");
        std::fs::remove_dir_all(root).unwrap();
    }
}
