//! Concrete recurrence gates for PE40; source checks never establish runtime acceptance.
use crate::*;
use std::collections::BTreeSet;
#[path = "platform_fitness/selftest.rs"]
mod rejection_fixtures;
pub(crate) fn selftest() -> Result<Value> {
    rejection_fixtures::run()
}
fn yaml(path: &Path) -> Result<Value> {
    let mut v: serde_yaml::Value = serde_yaml::from_slice(&std::fs::read(path)?)?;
    v.apply_merge()?;
    Ok(serde_json::to_value(v)?)
}
fn permissions(v: &Value) -> bool {
    v.as_object().is_some()
        && v.as_object()
            .unwrap()
            .values()
            .all(|v| matches!(v.as_str(), Some("read" | "write" | "none")))
}
fn workflow(v: &Value) -> Result<()> {
    require(
        permissions(&v["permissions"]),
        "explicit workflow permission map required; broad string grants forbidden",
    )?;
    for job in v["jobs"]
        .as_object()
        .ok_or("workflow jobs missing")?
        .values()
    {
        if let Some(p) = job.get("permissions") {
            require(
                permissions(p),
                "explicit bounded job permission map required",
            )?;
        }
        let uses = job.get("uses").into_iter().chain(
            job["steps"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s.get("uses")),
        );
        for u in uses {
            let u = u.as_str().ok_or("invalid workflow reference")?;
            require(
                !u.starts_with("$/"),
                "GitHub local references require standard ./ syntax",
            )?;
            if u.starts_with("./") {
                require(
                    !Path::new(u)
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir)),
                    "local action escapes repository",
                )?;
                continue;
            }
            let (name, revision) = u
                .rsplit_once('@')
                .ok_or("external workflow action is not SHA-pinned")?;
            require(
                name.contains('/')
                    && revision.len() == 40
                    && revision
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "external workflow action is not SHA-pinned",
            )?;
        }
    }
    Ok(())
}
fn provider_credential(key: &str) -> bool {
    let key = key
        .strip_prefix("${")
        .and_then(|s| s.strip_suffix('}'))
        .filter(|s| !s.contains("${"))
        .map(|s| s.split_once(":?").map(|p| p.0).unwrap_or(s))
        .unwrap_or(key);
    if matches!(
        key,
        "KNOWLEDGE_QDRANT__API_KEY" | "KNOWLEDGE_QDRANT__API_KEY_FILE"
    ) {
        return false;
    }
    key.contains("API_KEY")
        || key.contains("GOOGLE_APPLICATION_CREDENTIALS")
        || (key.contains("PROVIDER")
            && ["KEY", "TOKEN", "SECRET", "CREDENTIAL", "CONFIG"]
                .iter()
                .any(|s| key.contains(s)))
}
fn provider_boundary(config: &Value) -> Result<()> {
    for (name, service) in config["services"]
        .as_object()
        .ok_or("Compose services missing")?
    {
        if !(name.contains("knowledge") || name.contains("sdk")) {
            continue;
        }
        require(
            service
                .get("env_file")
                .is_none_or(|v| v.is_null() || v.as_array().is_some_and(Vec::is_empty)),
            "Knowledge/SDK cannot import an opaque environment file",
        )?;
        for field in ["secrets", "configs", "volumes"] {
            for input in service[field].as_array().into_iter().flatten() {
                let names: Vec<&str> = if let Some(s) = input.as_str() {
                    vec![s]
                } else {
                    ["source", "target"]
                        .iter()
                        .filter_map(|k| input[*k].as_str())
                        .collect()
                };
                require(
                    !names.iter().any(|name| {
                        provider_credential(
                            &name.to_ascii_uppercase().replace(['-', '/', '.'], "_"),
                        )
                    }),
                    "provider credential file/reference in Knowledge/SDK configuration",
                )?;
            }
        }
        let mut keys = Vec::new();
        match &service["environment"] {
            Value::Object(env) => {
                for (key, value) in env {
                    keys.push((key.as_str(), value.as_str() == Some("")));
                }
            }
            Value::Array(env) => {
                for item in env {
                    let raw = item
                        .as_str()
                        .ok_or("invalid environment entry; values withheld")?;
                    let (key, value) = raw
                        .split_once('=')
                        .map(|(k, v)| (k, Some(v)))
                        .unwrap_or((raw, None));
                    keys.push((key, value == Some("")));
                }
            }
            Value::Null => (),
            _ => return Err("invalid service environment; values withheld".into()),
        }
        for (key, empty) in keys {
            let key = key.to_ascii_uppercase();
            let provider = provider_credential(&key);
            require(
                !provider || empty,
                "provider credential in Knowledge/SDK environment",
            )?;
        }
    }
    Ok(())
}
fn provider_sources(root: &Path) -> Result<()> {
    provider_boundary(&yaml(&root.join("docker-compose.yml"))?)?;
    for directory in ["deploy", "docker"] {
        if !root.join(directory).is_dir() {
            continue;
        }
        for relative in walk(&root.join(directory), crate::harness::PRUNED, false)? {
            if relative
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|name| {
                    name.contains("compose") && (name.ends_with(".yml") || name.ends_with(".yaml"))
                })
            {
                let config = yaml(&root.join(directory).join(relative))?;
                if config["services"].is_object() {
                    provider_boundary(&config)?;
                }
            }
        }
    }
    Ok(())
}
fn coordination(v: &Value) -> Result<()> {
    let command = &v["services"]["redis"]["command"];
    let tokens: Vec<String> = if let Some(text) = command.as_str() {
        text.split_whitespace().map(str::to_owned).collect()
    } else {
        command
            .as_array()
            .ok_or("coordination Redis command missing")?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or("invalid coordination command")
            })
            .collect::<std::result::Result<_, _>>()?
    };
    let policies: Vec<_> = tokens
        .iter()
        .enumerate()
        .filter_map(|(i, token)| {
            if token == "--maxmemory-policy" {
                Some(tokens.get(i + 1).map(String::as_str).unwrap_or(""))
            } else {
                token.strip_prefix("--maxmemory-policy=")
            }
        })
        .collect();
    require(
        tokens
            .first()
            .is_some_and(|s| s == "redis-server" || s.ends_with("/redis-server"))
            && policies == ["noeviction"],
        "coordination Redis requires exactly one effective noeviction policy",
    )?;
    qdrant_auth(v)
}
fn qdrant_auth(v: &Value) -> Result<()> {
    let key = &v["services"]["qdrant"]["environment"]["QDRANT__SERVICE__API_KEY"];
    require(
        key.as_str()
            .is_some_and(|s| !s.is_empty() && (!s.starts_with("${") || s.contains(":?"))),
        "Qdrant API key must be required, never optional or empty",
    )
}
fn coordination_source(root: &Path, v: &Value) -> Result<()> {
    if coordination(v).is_ok() {
        return Ok(());
    }
    let service = &v["services"]["redis"];
    let command = service["command"]
        .as_array()
        .ok_or("coordination command must be explicit")?;
    require(
        command.len() == 2 && command[0] == "/bin/sh" && command[1].as_str().is_some(),
        "unreviewed coordination command wrapper",
    )?;
    let target = command[1].as_str().unwrap();
    let source = service["volumes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find_map(|v| v.strip_suffix(&format!(":{target}:ro")))
        .ok_or("coordination entrypoint needs a readonly source bind")?;
    let path = root.join(source).canonicalize()?;
    require(
        path.starts_with(root.canonicalize()?),
        "coordination entrypoint escapes repository",
    )?;
    let text = std::fs::read_to_string(path)?;
    let code = text
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let policies: Vec<_> = regex::Regex::new(r"maxmemory-policy\s+([a-z][a-z-]*)")?
        .captures_iter(&code)
        .map(|m| m[1].to_owned())
        .collect();
    require(
        policies == ["noeviction"]
            && code
                .lines()
                .any(|line| line.trim() == "printf '%s\\n' 'maxmemory-policy noeviction'")
            && code.contains("} > \"$runtime_config\"")
            && code.lines().any(|line| {
                line.trim()
                    == "exec /usr/local/bin/docker-entrypoint.sh redis-server \"$runtime_config\""
            }),
        "coordination entrypoint must emit exactly one noeviction policy into its executed config",
    )?;
    qdrant_auth(v)
}
fn monitoring_service(v: &Value) -> Result<()> {
    let user = v["user"]
        .as_str()
        .ok_or("monitoring non-root UID:GID missing")?;
    require(
        user.split(':').count() == 2
            && user
                .split(':')
                .all(|s| s.parse::<u32>().is_ok_and(|v| v > 0)),
        "monitoring requires explicit numeric non-root UID:GID",
    )?;
    require(
        v["read_only"] == true
            && v["pids_limit"]
                .as_u64()
                .is_some_and(|v| (1..=4096).contains(&v)),
        "monitoring read-only root/PID bound missing",
    )?;
    require(
        v["cap_drop"]
            .as_array()
            .is_some_and(|v| v == &vec![json!("ALL")])
            && v.get("cap_add").is_none(),
        "monitoring capabilities must all be dropped",
    )?;
    require(
        v["security_opt"]
            .as_array()
            .is_some_and(|v| v.iter().any(|s| s == "no-new-privileges:true")),
        "monitoring no-new-privileges missing",
    )?;
    require(
        v["tmpfs"].as_array().is_some_and(|v| {
            v.iter().filter_map(Value::as_str).any(|s| {
                s.starts_with("/tmp:")
                    && ["noexec", "nosuid", "nodev", "size="]
                        .iter()
                        .all(|v| s.contains(v))
            })
        }),
        "monitoring bounded noexec scratch missing",
    )?;
    require(
        v["networks"]
            .as_array()
            .is_none_or(|v| !v.iter().any(|s| s == "edge")),
        "monitoring store must never join edge",
    )?;
    Ok(())
}
fn dependency_manifest(v: &toml::Value) -> Result<()> {
    let name = v
        .get("package")
        .and_then(|v| v.get("name"))
        .and_then(toml::Value::as_str)
        .unwrap_or("");
    let mut tables = Vec::new();
    dependency_tables(v, &mut tables);
    for deps in tables {
        for (alias, spec) in deps {
            dependency_rule(
                name,
                spec.get("package")
                    .and_then(toml::Value::as_str)
                    .unwrap_or(alias),
            )?;
        }
    }
    Ok(())
}
fn dependency_tables<'a>(
    v: &'a toml::Value,
    out: &mut Vec<&'a toml::map::Map<String, toml::Value>>,
) {
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = v.get(key).and_then(toml::Value::as_table) {
            out.push(table);
        }
    }
    if let Some(targets) = v.get("target").and_then(toml::Value::as_table) {
        for target in targets.values() {
            dependency_tables(target, out);
        }
    }
}
fn resolved_dependency_manifest(root: &Path, manifest: &Path) -> Result<()> {
    let value: toml::Value = toml::from_str(&std::fs::read_to_string(root.join(manifest))?)?;
    let name = value
        .get("package")
        .and_then(|v| v.get("name"))
        .and_then(toml::Value::as_str)
        .unwrap_or("");
    if !name.contains("knowledge") && !name.ends_with("-contract") && !name.ends_with("-contracts")
    {
        return Ok(());
    }
    let mut tables = Vec::new();
    dependency_tables(&value, &mut tables);
    for deps in tables {
        for (alias, spec) in deps {
            let target = crate::architecture::dependency_target(root, manifest, alias, spec, None)?;
            dependency_rule(name, &target)?;
        }
    }
    Ok(())
}
fn dependency_rule(name: &str, target: &str) -> Result<()> {
    require(
        !(name.contains("knowledge") && target == "ai-platform-gateway"),
        "Knowledge must not depend on Gateway service authority",
    )?;
    require(
        !(name.ends_with("-contract") || name.ends_with("-contracts"))
            || ![
                "tokio", "reqwest", "sqlx", "redis", "tonic", "axum", "hyper",
            ]
            .contains(&target),
        "pure contract crate has I/O dependency",
    )
}
fn base_images(text: &str) -> Result<()> {
    let mut arguments = BTreeMap::new();
    let mut stages = BTreeSet::new();
    let mut global_arguments = true;
    for line in text.lines().map(str::trim).filter(|s| !s.starts_with('#')) {
        if let Some(arg) = line.strip_prefix("ARG ").filter(|_| global_arguments) {
            if let Some((key, value)) = arg.split_once('=') {
                arguments.insert(key, value.trim_matches('"'));
            }
        }
        if !line.to_ascii_uppercase().starts_with("FROM ") {
            continue;
        }
        global_arguments = false;
        let words: Vec<_> = line.split_whitespace().collect();
        let mut image = *words
            .iter()
            .skip(1)
            .find(|v| !v.starts_with("--"))
            .ok_or("Docker base missing")?;
        let mut expanded = BTreeSet::new();
        while let Some(key) = image.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
            require(expanded.insert(key), "cyclic Docker base argument")?;
            image = arguments
                .get(key)
                .copied()
                .ok_or("Docker base argument has no supported default")?;
        }
        if image != "scratch" && !stages.contains(image) {
            require(!image.contains("${"), "unresolved Docker base argument")?;
            let tagged = image.split('@').next().unwrap();
            let repository = tagged.rsplit('/').next().unwrap();
            let (name, tag) = repository.split_once(':').unwrap_or((repository, "latest"));
            require(
                tag != "latest" && !tag.is_empty(),
                "mutable/implicit latest base image",
            )?;
            let version: Vec<_> = tag.split(['.', '-']).collect();
            let major = version[0].parse::<u32>().ok();
            let minor = version.get(1).and_then(|s| s.parse::<u32>().ok());
            require(
                !matches!(name, "nginx" | "nginx-unprivileged")
                    || major.zip(minor).is_some_and(|v| v >= (1, 30)),
                "unsupported nginx deployment image line",
            )?;
            require(
                name != "python" || major.zip(minor).is_some_and(|v| v >= (3, 11)),
                "unsupported Python deployment image line",
            )?;
            require(
                !["buster", "bullseye"]
                    .iter()
                    .any(|v| tag.split('-').any(|p| p == *v))
                    && !(name == "debian" && major.is_some_and(|v| v <= 11)),
                "unsupported Debian deployment image line",
            )?;
        }
        if let Some(index) = words.iter().position(|s| s.eq_ignore_ascii_case("AS")) {
            stages.insert(*words.get(index + 1).ok_or("Docker stage alias missing")?);
        }
    }
    Ok(())
}
fn license_alignment(root: &Path) -> Result<()> {
    let license = std::fs::read_to_string(root.join("LICENSE"))?;
    let notice = std::fs::read_to_string(root.join("NOTICE"))?;
    require(
        license.contains("Apache License")
            && license.contains("Version 2.0")
            && notice.contains("Apache License, Version 2.0")
            && notice.contains("OpenAI"),
        "first-party license/NOTICE alignment missing",
    )?;
    let manifest: toml::Value = toml::from_str(&std::fs::read_to_string(
        root.join("rust/platform-native/Cargo.toml"),
    )?)?;
    require(
        manifest["workspace"]["package"]["license"].as_str() == Some("Apache-2.0"),
        "native workspace license differs from root",
    )?;
    let dependency = object(&root.join("deploy/security/dependency-policy.json"))?;
    let deny: toml::Value = toml::from_str(&std::fs::read_to_string(
        root.join("deploy/security/deny.toml"),
    )?)?;
    require(
        deny["advisories"]["ignore"]
            .as_array()
            .is_some_and(Vec::is_empty)
            && deny["licenses"]["exceptions"]
                .as_array()
                .is_some_and(Vec::is_empty)
            && deny["bans"]["skip"].as_array().is_some_and(Vec::is_empty)
            && deny["bans"]["skip-tree"]
                .as_array()
                .is_some_and(Vec::is_empty)
            && deny["graph"]["exclude"]
                .as_array()
                .is_some_and(Vec::is_empty),
        "dependency gate must not bypass unresolved licenses/advisories",
    )?;
    require(
        dependency["blocked_packages"]
            .as_array()
            .is_some_and(|v| v.contains(&json!("serde_yaml")) && v.contains(&json!("serde_yml")))
            && dependency["blocked_prefixes"]
                .as_array()
                .is_some_and(|v| v.contains(&json!("rustpython-"))),
        "retired dependency bans missing",
    )?;
    Ok(())
}
fn rendered_kubernetes(docs: &[Value]) -> Result<usize> {
    let mut guarded = BTreeSet::new();
    let mut frontend_references = BTreeSet::new();
    let mut checked = 0;
    for d in docs {
        let kind = d["kind"].as_str().unwrap_or("");
        if ["Deployment", "StatefulSet", "Rollout"].contains(&kind) {
            let component = d["spec"]["selector"]["matchLabels"]["app.kubernetes.io/component"]
                .as_str()
                .or_else(|| d["metadata"]["labels"]["app.kubernetes.io/component"].as_str())
                .unwrap_or("");
            if component.starts_with("gateway") || component == "agent-runtime" {
                require(
                    kind != "Rollout" && d["spec"]["replicas"].as_u64() == Some(1),
                    "Gateway/Runtime singleton may not be scaled or canaried before original HA acceptance",
                )?;
                guarded.insert(
                    d["metadata"]["name"]
                        .as_str()
                        .ok_or("guarded workload name missing")?
                        .to_owned(),
                );
            }
            if kind == "Rollout" {
                require(
                    component == "frontend",
                    "only stateless frontend canary is admitted",
                )?;
                frontend_references.insert(
                    d["spec"]["workloadRef"]["name"]
                        .as_str()
                        .ok_or("frontend Rollout workloadRef missing")?
                        .to_owned(),
                );
            }
            checked += 1;
        }
    }
    for reference in &frontend_references {
        require(
            docs.iter()
                .filter(|d| {
                    d["kind"] == "Deployment" && d["metadata"]["name"] == reference.as_str()
                })
                .count()
                == 1,
            "frontend workloadRef must resolve exactly one rendered Deployment",
        )?;
    }
    for d in docs {
        if d["kind"] == "Deployment"
            && d["metadata"]["name"]
                .as_str()
                .is_some_and(|n| frontend_references.contains(n))
        {
            require(
                d["spec"]["replicas"].as_u64() == Some(0),
                "workloadRef source Deployment must stay at zero to prevent bypassing frontend canary",
            )?;
        }
        if d["kind"] == "HorizontalPodAutoscaler" {
            require(
                !d["spec"]["scaleTargetRef"]["name"]
                    .as_str()
                    .is_some_and(|n| frontend_references.contains(n)),
                "source frontend Deployment HPA would bypass canary",
            )?;
            require(
                !d["spec"]["scaleTargetRef"]["name"]
                    .as_str()
                    .is_some_and(|s| guarded.contains(s)),
                "HPA cannot bypass Gateway/Runtime singleton guard",
            )?;
        }
    }
    let admission = docs.iter().find(|d| {
        d["kind"] == "Job"
            && d["spec"]["template"]["metadata"]["labels"]["app.kubernetes.io/component"]
                == "delivery-admission"
    });
    if let Some(admission) = admission {
        let wave = admission["metadata"]["annotations"]["argocd.argoproj.io/sync-wave"]
            .as_str()
            .ok_or("signature admission sync wave missing")?
            .parse::<i32>()?;
        for job in docs.iter().filter(|d| {
            d["kind"] == "Job"
                && d["metadata"]["labels"]["app.kubernetes.io/component"] == "migration"
        }) {
            let a = &job["metadata"]["annotations"];
            let migration_wave = a["argocd.argoproj.io/sync-wave"]
                .as_str()
                .or_else(|| a["helm.sh/hook-weight"].as_str())
                .unwrap_or("0")
                .parse::<i32>()?;
            require(
                wave < migration_wave,
                "signature admission must finish before any migration hook",
            )?;
        }
    }
    require(checked > 0, "no Kubernetes workload checked")?;
    Ok(checked)
}
pub(crate) fn source(root: &Path) -> Result<Value> {
    run(&Args {
        values: BTreeMap::from([("--repo-root".into(), root.to_string_lossy().into_owned())]),
        positionals: vec!["platform-fitness".into(), "source".into()],
        ..Default::default()
    })
}
pub fn run(args: &Args) -> Result<Value> {
    require(
        args.flags.is_empty() && args.positionals.len() <= 2,
        "fitness takes no bypass flags",
    )?;
    require(
        matches!(
            args.positionals.get(1).map(String::as_str),
            None | Some("source" | "rendered-compose" | "rendered-kubernetes")
        ),
        "unsupported fitness action",
    )?;
    for (name, values) in &args.multiple {
        require(
            [
                "--repo-root",
                "--evidence-out",
                "--config",
                "--mode",
                "--role-prefix",
            ]
            .contains(&name.as_str())
                && values.len() == 1,
            "unknown or repeated fitness option",
        )?;
    }
    let root = args.root();
    if args.positionals.get(1).map(String::as_str) == Some("rendered-compose") {
        let v = yaml(Path::new(args.get("--config")?))?;
        let mode = args
            .values
            .get("--mode")
            .map(String::as_str)
            .unwrap_or("full");
        crate::compose_identity::check(&root, &v, mode, false)?;
        crate::container_security::check(&v)?;
        crate::database_identity::check_compose(
            &v,
            args.values
                .get("--role-prefix")
                .map(String::as_str)
                .unwrap_or("ai_gateway_"),
        )?;
        provider_boundary(&v)?;
        return Ok(
            json!({"gate":"platform-fitness-rendered-compose","result":"pass","runtime_acceptance":false}),
        );
    }
    if args.positionals.get(1).map(String::as_str) == Some("rendered-kubernetes") {
        use serde::Deserialize;
        let text = std::fs::read_to_string(args.get("--config")?)?;
        let docs = serde_yaml::Deserializer::from_str(&text)
            .map(Value::deserialize)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        return Ok(
            json!({"gate":"platform-fitness-singleton-kubernetes","result":"pass","workloads":rendered_kubernetes(&docs)?,"ha_acceptance":false}),
        );
    }
    let mut checks = Vec::new();
    let mut record = |name: &str, result: Result<()>| {
        checks.push(match result {
            Ok(()) => json!({"check":name,"result":"pass"}),
            Err(e) => json!({"check":name,"result":"fail","reason":e.to_string()}),
        });
    };
    record(
        "shared-workload-source",
        crate::source_sync::verify_if_present(&root),
    );
    record(
        "image-identity-and-pairing-source",
        crate::compose_identity::source_contract(&root),
    );
    record(
        "scoped-database-source",
        crate::database_identity::check_source(&yaml(&root.join("docker-compose.yml"))?)
            .map(|_| ()),
    );
    record(
        "container-hardening-source",
        crate::container_security::run(&Args {
            values: BTreeMap::from([("--repo-root".into(), root.to_string_lossy().into_owned())]),
            positionals: vec!["container-security".into(), "source".into()],
            ..Default::default()
        })
        .map(|_| ()),
    );
    record(
        "observability-profile-hardening",
        (move || {
            for (path, names) in [
                (
                    "docker/monitoring/docker-compose.monitoring.yml",
                    vec![
                        "otel-collector",
                        "prometheus",
                        "grafana",
                        "loki",
                        "alertmanager",
                        "tempo",
                    ],
                ),
                (
                    "docker/monitoring/docker-compose.profiling.yml",
                    vec!["pyroscope"],
                ),
            ] {
                let v = yaml(&root.join(path))?;
                for name in names {
                    monitoring_service(&v["services"][name])?;
                }
            }
            Ok(())
        })(),
    );
    let root = args.root();
    record(
        "workflow-sha-and-permissions",
        (move || {
            for relative in walk(&root.join(".github"), crate::harness::PRUNED, false)? {
                if relative.starts_with("actions")
                    && relative
                        .file_name()
                        .is_some_and(|s| s == "action.yml" || s == "action.yaml")
                {
                    let action = yaml(&root.join(".github").join(relative))?;
                    if action["runs"]["using"] == "composite" {
                        workflow(
                            &json!({"permissions":{},"jobs":{"composite":{"steps":action["runs"]["steps"]}}}),
                        )?;
                    }
                }
            }
            for entry in std::fs::read_dir(root.join(".github/workflows"))? {
                let path = entry?.path();
                if path.extension().is_some_and(|s| s == "yml" || s == "yaml") {
                    require(
                        std::fs::symlink_metadata(&path)?.is_file(),
                        "workflow symlink refused",
                    )?;
                    workflow(&yaml(&path)?)?;
                }
            }
            Ok(())
        })(),
    );
    let root = args.root();
    record(
        "coordination-noeviction-and-qdrant-auth",
        crate::valkey_deployment::check(&root)
            .map_err(Into::into)
            .and_then(|_| coordination_source(&root, &yaml(&root.join("docker-compose.yml"))?)),
    );
    record(
        "qdrant-auth-source",
        qdrant_auth(&yaml(&root.join("docker-compose.yml"))?),
    );
    record("provider-credential-boundary", provider_sources(&root));
    record(
        "license-and-retired-dependency-policy",
        license_alignment(&root),
    );
    record(
        "knowledge-and-contract-dependency-boundaries",
        (move || {
            for path in walk(&root.join("rust"), crate::harness::PRUNED, false)? {
                if path.file_name().is_some_and(|s| s == "Cargo.toml") {
                    resolved_dependency_manifest(&root, &Path::new("rust").join(path))?;
                }
            }
            Ok(())
        })(),
    );
    let root = args.root();
    record(
        "supported-production-bases",
        (move || {
            for path in [
                "deploy/platform-native/Dockerfile",
                "web/Dockerfile",
                "web/Dockerfile.artifact",
                "docker/code-interpreter/Dockerfile",
                "deploy/agent-runtime-source/Dockerfile.runtime",
                "deploy/agent-runtime-source/Dockerfile.capability-worker",
            ] {
                base_images(&std::fs::read_to_string(root.join(path))?)?;
            }
            Ok(())
        })(),
    );
    let root = args.root();
    record(
        "gitops-policy-snapshot",
        (move || {
            for name in ["supply-chain-policy.json", "dependency-policy.json"] {
                require(
                    digest(&root.join("deploy/security").join(name))?
                        == digest(&root.join("deploy/gitops/chart/files").join(name))?,
                    "GitOps admission policy snapshot differs from current repository policy",
                )?;
            }
            let v = yaml(&root.join("deploy/gitops/chart/values.yaml"))?;
            require(
                v["enabled"] == false && v["canary"]["enabled"] == false,
                "candidate GitOps must not default to deployment",
            )?;
            Ok(())
        })(),
    );
    let passed = checks.iter().all(|c| c["result"] == "pass");
    let result = json!({"gate":"platform-fitness","result":if passed{"pass"}else{"fail"},"checks":checks,"runtime_acceptance":false,"rendered_configuration_acceptance":false});
    if let Some(path) = args.values.get("--evidence-out") {
        write_json(Path::new(path), &result)?;
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_recurring_finding_has_a_real_production_validator_rejection() {
        let report = selftest().unwrap();
        assert_eq!(report["result"], "pass");
        assert_eq!(report["findings"].as_array().unwrap().len(), 12);
    }
    #[test]
    fn eviction_missing_qdrant_key_and_superuser_dsn_are_negative_gates() {
        let good = json!({"services":{"redis":{"command":["redis-server","--maxmemory-policy","noeviction"]},"qdrant":{"environment":{"QDRANT__SERVICE__API_KEY":"${QDRANT_API_KEY:?required}"}}}});
        coordination(&good).unwrap();
        let mut bad = good.clone();
        bad["services"]["redis"]["command"] =
            json!(["redis-server", "--maxmemory-policy", "allkeys-lru"]);
        assert!(coordination(&bad).is_err());
        let mut bad = good;
        bad["services"]["qdrant"]["environment"]["QDRANT__SERVICE__API_KEY"] =
            json!("${QDRANT_API_KEY:-}");
        assert!(coordination(&bad).is_err());
        assert!(crate::database_identity::check_compose(&json!({"services":{"gateway":{"environment":{"DATABASE_URL":"postgresql://postgres:fixture@postgres/db"}}}}),"ai_gateway_").is_err());
    }
    #[test]
    fn tag_pins_and_missing_or_broad_permissions_fail() {
        let good = json!({"permissions":{"contents":"read"},"jobs":{"test":{"steps":[{"uses":format!("actions/checkout@{}","a".repeat(40))}]}}});
        workflow(&good).unwrap();
        for bad in [
            json!({"jobs":{}}),
            json!({"permissions":"write-all","jobs":{}}),
            json!({"permissions":{},"jobs":{"test":{"steps":[{"uses":"actions/checkout@v4"}]}}}),
        ] {
            assert!(workflow(&bad).is_err());
        }
    }
    #[test]
    fn provider_credentials_and_renamed_dependency_aliases_fail() {
        assert!(provider_boundary(&json!({"services":{"knowledge-service":{"environment":{"OPENAI_API_KEY":"seed-private"}}}})).is_err());
        assert!(provider_boundary(&json!({"services":{"sdk-agent-worker":{"environment":["OPENAI_API_KEY_FILE=/private/key"]}}})).is_err());
        for text in [
            "[package]\nname='ai-platform-knowledge-domain'\n[dependencies]\nrenamed={package='ai-platform-gateway',version='1'}",
            "[package]\nname='ai-platform-contracts'\n[dependencies]\nnetwork={package='reqwest',version='1'}",
        ] {
            assert!(dependency_manifest(&toml::from_str(text).unwrap()).is_err());
        }
    }
    #[test]
    fn each_monitoring_hardening_regression_fails() {
        let good = json!({"user":"10001:10001","read_only":true,"pids_limit":128,"cap_drop":["ALL"],"security_opt":["no-new-privileges:true"],"tmpfs":["/tmp:rw,noexec,nosuid,nodev,size=16m"],"networks":["app"]});
        monitoring_service(&good).unwrap();
        for key in [
            "user",
            "read_only",
            "pids_limit",
            "cap_drop",
            "security_opt",
            "tmpfs",
        ] {
            let mut bad = good.clone();
            bad.as_object_mut().unwrap().remove(key);
            assert!(monitoring_service(&bad).is_err(), "{key}");
        }
        let mut bad = good;
        bad["networks"] = json!(["edge"]);
        assert!(monitoring_service(&bad).is_err());
    }
    #[test]
    fn unsupported_base_lines_are_not_silently_grandfathered() {
        base_images("FROM debian:trixie-slim AS native-base").unwrap();
        for s in [
            "FROM nginx:1.27-alpine",
            "FROM python:3.9-slim",
            "FROM debian:bullseye",
            "FROM vendor/tool:latest",
            "ARG BASE=nginx:1.27-alpine\nFROM ${BASE}",
        ] {
            assert!(base_images(s).is_err());
        }
    }
    #[test]
    fn migration_cannot_run_before_signed_artifact_admission() {
        let workload = json!({"kind":"Deployment","metadata":{"name":"gateway"},"spec":{"replicas":1,"selector":{"matchLabels":{"app.kubernetes.io/component":"gateway"}}}});
        let migration = json!({"kind":"Job","metadata":{"labels":{"app.kubernetes.io/component":"migration"},"annotations":{"helm.sh/hook-weight":"-5"}}});
        let mut verifier = json!({"kind":"Job","metadata":{"annotations":{"argocd.argoproj.io/sync-wave":"-25"}},"spec":{"template":{"metadata":{"labels":{"app.kubernetes.io/component":"delivery-admission"}}}}});
        rendered_kubernetes(&[workload.clone(), migration.clone(), verifier.clone()]).unwrap();
        verifier["metadata"]["annotations"]["argocd.argoproj.io/sync-wave"] = json!("-2");
        assert!(rendered_kubernetes(&[workload, migration, verifier]).is_err());
    }
    #[test]
    fn singleton_replica_canary_and_hpa_bypass_fail() {
        let workload = json!({"kind":"Deployment","metadata":{"name":"gateway"},"spec":{"replicas":1,"selector":{"matchLabels":{"app.kubernetes.io/component":"gateway"}}}});
        rendered_kubernetes(&[workload.clone()]).unwrap();
        let mut bad = workload.clone();
        bad["spec"]["replicas"] = json!(2);
        assert!(rendered_kubernetes(&[bad]).is_err());
        let mut bad = workload.clone();
        bad["kind"] = json!("Rollout");
        assert!(rendered_kubernetes(&[bad]).is_err());
        assert!(rendered_kubernetes(&[workload,json!({"kind":"HorizontalPodAutoscaler","spec":{"scaleTargetRef":{"name":"gateway"}}})]).is_err());
    }
}
