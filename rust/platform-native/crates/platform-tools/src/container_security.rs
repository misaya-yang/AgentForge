//! Compose container/network/Qdrant admission. Never reads private env files.
use crate::*;
use std::collections::BTreeSet;
fn list(value: &Value) -> BTreeSet<String> {
    if let Some(map) = value.as_object() {
        map.keys().cloned().collect()
    } else {
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    }
}
fn nonroot(value: &Value) -> bool {
    let Some(user) = value.as_str() else {
        return false;
    };
    let ids: Vec<_> = user.split(':').collect();
    ids.len() == 2
        && ids
            .iter()
            .all(|id| id.parse::<u32>().is_ok_and(|id| id > 0))
}
fn input_identity(value: &str) -> &str {
    value
        .strip_prefix("${")
        .and_then(|v| v.split_once(":?").map(|(name, _)| name))
        .unwrap_or(value)
}
fn required_secret(value: &str) -> bool {
    if value.starts_with("${") {
        value.ends_with('}') && value.contains(":?")
    } else {
        !value.is_empty()
    }
}
fn mount(service: &Value, target: &str) -> bool {
    service["volumes"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|v| {
            v["target"] == target
                && v["type"] == "bind"
                && v["read_only"] == true
                && v["bind"]["create_host_path"] == false
                && v["source"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty() && (!s.starts_with("${") || required_secret(s)))
        })
}
fn owners(network: &str, service: &str) -> bool {
    match network {
        "edge" => ["frontend", "gateway"].contains(&service),
        "app" => ![
            "postgres",
            "redis",
            "qdrant",
            "object-store",
            "frontend",
            "database-bootstrap",
            "migrate",
            "gateway-init",
        ]
        .contains(&service),
        "data" => ["qdrant", "knowledge-service", "knowledge-worker"].contains(&service),
        "data-postgres" => [
            "postgres",
            "database-bootstrap",
            "migrate",
            "gateway",
            "knowledge-service",
            "knowledge-worker",
            "agent-runtime",
            "agent-capability-worker",
        ]
        .contains(&service),
        "data-redis" => {
            ["redis", "gateway", "knowledge-service", "knowledge-worker"].contains(&service)
        }
        "data-object-store" => [
            "object-store",
            "gateway",
            "knowledge-service",
            "knowledge-worker",
            "agent-runtime",
            "agent-capability-worker",
        ]
        .contains(&service),
        _ => false,
    }
}
fn scratch(service: &Value) -> bool {
    service["tmpfs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|s| {
            let Some((path, options)) = s.split_once(':') else {
                return false;
            };
            let opts: BTreeSet<_> = options.split(',').collect();
            path == "/tmp"
                && ["rw", "noexec", "nosuid", "nodev"]
                    .iter()
                    .all(|v| opts.contains(v))
                && opts.iter().any(|v| v.starts_with("size="))
        })
}

fn volume_parts(value: &Value) -> Result<(&str, &str, bool)> {
    if let Some(text) = value.as_str() {
        let (head, tail) = text.rsplit_once(':').ok_or("volume target missing")?;
        if tail == "ro" || tail == "rw" {
            let (source, target) = head.rsplit_once(':').ok_or("volume source missing")?;
            Ok((source, target, tail == "ro"))
        } else {
            Ok((head, tail, false))
        }
    } else {
        Ok((
            value["source"].as_str().unwrap_or(""),
            value["target"].as_str().unwrap_or(""),
            value["read_only"] == true,
        ))
    }
}
fn sandbox_seam(config: &Value) -> Result<()> {
    let services = config["services"]
        .as_object()
        .ok_or("Compose services missing")?;
    let Some(control) = services.get("agent-capability-worker") else {
        require(
            !services.contains_key("agent-python-sandbox"),
            "sandbox requires its owning control service",
        )?;
        return Ok(());
    };
    let sandbox = services
        .get("agent-python-sandbox")
        .ok_or("control must use a separate sandbox service")?;
    require(
        control["user"] == "10001:10001" && sandbox["user"] == control["user"],
        "UDS owners must use matching explicit non-root UID/GID",
    )?;
    require(
        sandbox["network_mode"] == "none" && list(&sandbox["networks"]).is_empty(),
        "sandbox must have no container network",
    )?;
    for key in [
        "env_file",
        "secrets",
        "configs",
        "ports",
        "expose",
        "extra_hosts",
        "dns",
        "links",
        "external_links",
        "volumes_from",
    ] {
        require(
            sandbox.get(key).is_none(),
            "sandbox may not inherit external inputs or TCP endpoints",
        )?;
    }
    let allowed_env = [
        "PATH",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TZ",
        "HOME",
        "HOSTNAME",
        "TERM",
        "RUST_LOG",
        "RUST_BACKTRACE",
        "PYTHONNOUSERSITE",
        "PYTHONDONTWRITEBYTECODE",
        "PYTHONUNBUFFERED",
        "PYTHON_VERSION",
        "PYTHON_SHA256",
        "GPG_KEY",
    ];
    require(
        sandbox.get("environment").is_none()
            || sandbox["environment"]
                .as_object()
                .is_some_and(|map| map.keys().all(|key| allowed_env.contains(&key.as_str()))),
        "sandbox environment must be credential-free with no proxy/platform env",
    )?;
    require(
        sandbox["entrypoint"] == json!(["ai-platform-python-sandbox"]) && sandbox["init"] == true,
        "sandbox must use the independent executable with init",
    )?;
    require(
        sandbox["image"].as_str().is_some_and(required_secret)
            && sandbox["image"] != control["image"],
        "sandbox requires its separately admitted image",
    )?;
    let command = sandbox["command"]
        .as_array()
        .ok_or("sandbox command must be an explicit public argument array")?;
    require(
        command.len() % 2 == 0,
        "sandbox command arguments incomplete",
    )?;
    let mut args = std::collections::BTreeMap::new();
    for pair in command.chunks_exact(2) {
        let flag = pair[0].as_str().ok_or("sandbox argument invalid")?;
        let value = pair[1].as_str().ok_or("sandbox argument invalid")?;
        require(
            [
                "--public-key-id",
                "--public-key-hex",
                "--socket",
                "--workspace-root",
                "--journal-root",
                "--cgroup-root",
                "--package-policy",
            ]
            .contains(&flag)
                && args.insert(flag, value).is_none(),
            "sandbox command contains an unknown/private or duplicate argument",
        )?;
    }
    let arg = |key: &str| args.get(key).copied().unwrap_or("");
    require(
        arg("--socket") == "/run/agentforge-sandbox/control.sock"
            && arg("--workspace-root") == "/workspace"
            && arg("--journal-root") == "/var/lib/agentforge-sandbox/jobs"
            && arg("--cgroup-root") == "/sys/fs/cgroup/agentforge-sandbox",
        "sandbox UDS/workspace/journal/delegation paths are fixed",
    )?;
    let public = arg("--public-key-hex");
    require(
        (public.starts_with("${AGENT_SANDBOX_PUBLIC_KEY_HEX:?") && required_secret(public))
            || (public.len() == 64 && public.bytes().all(|b| b.is_ascii_hexdigit())),
        "sandbox must receive only the public Ed25519 verifier",
    )?;
    require(
        required_secret(arg("--public-key-id"))
            && input_identity(arg("--public-key-id"))
                == input_identity(
                    control["environment"]["AI_PLATFORM_SANDBOX_KEY_ID"]
                        .as_str()
                        .unwrap_or(""),
                ),
        "sandbox/control purpose key ids must match",
    )?;
    require(
        control["environment"]["AI_PLATFORM_SANDBOX_SOCKET"] == arg("--socket")
            && control["environment"]["AI_PLATFORM_SANDBOX_SIGNING_KEY_FILE"]
                == "/run/agentforge-sandbox-signing/signing.seed"
            && mount(control, "/run/agentforge-sandbox-signing/signing.seed"),
        "control must use the remote UDS and its private purpose-specific seed file",
    )?;
    require(
        control["depends_on"]["agent-python-sandbox"]["condition"] == "service_healthy",
        "control must await real UDS health",
    )?;
    require(
        !list(&control["security_opt"])
            .iter()
            .any(|value| value.contains("capability-worker-seccomp.json")),
        "control must not retain Python namespace seccomp exceptions",
    )?;
    require(
        list(&sandbox["security_opt"]).iter().any(|value| {
            matches!(
                value.as_str(),
                "seccomp=./deploy/security/capability-worker-seccomp.json"
                    | "seccomp=../security/capability-worker-seccomp.json"
            )
        }),
        "sandbox requires the reviewed namespace seccomp profile",
    )?;
    require(
        sandbox["healthcheck"]["test"]
            == json!([
                "CMD",
                "ai-platform-python-sandbox-health",
                "/run/agentforge-sandbox/control.sock"
            ]),
        "sandbox readiness must probe only the real UDS",
    )?;
    let mut required = BTreeSet::from([
        "/run/agentforge-sandbox",
        "/workspace",
        "/var/lib/agentforge-sandbox/jobs",
        "/sys/fs/cgroup/agentforge-sandbox",
    ]);
    if args.contains_key("--package-policy") {
        require(
            arg("--package-policy") == "/etc/agentforge/python-package-policy.json"
                && mount(sandbox, arg("--package-policy")),
            "package grants require their existing read-only public policy bind",
        )?;
        required.insert("/etc/agentforge/python-package-policy.json");
    }
    let mut seen = BTreeSet::new();
    for volume in sandbox["volumes"]
        .as_array()
        .ok_or("sandbox private volumes missing")?
    {
        let (source, target, read_only) = volume_parts(volume)?;
        require(
            required.contains(target) && seen.insert(target),
            "sandbox receives an unexpected/shared credential mount",
        )?;
        let expected = match target {
            "/run/agentforge-sandbox" => Some("sandbox-control-socket"),
            "/workspace" => Some("sandbox-private-workspaces"),
            "/var/lib/agentforge-sandbox/jobs" => Some("sandbox-private-journal"),
            _ => None,
        };
        if let Some(expected) = expected {
            require(
                source == expected && !read_only,
                "sandbox must own its distinct writable private volume",
            )?;
        }
        if target == "/sys/fs/cgroup/agentforge-sandbox" {
            require(
                source.starts_with("${")
                    || (Path::new(source).is_absolute()
                        && !["/", "/sys", "/sys/fs", "/sys/fs/cgroup"].contains(&source)
                        && !Path::new(source)
                            .components()
                            .any(|part| matches!(part, std::path::Component::ParentDir))),
                "sandbox delegation may not select the host cgroup hierarchy or a relative/traversal path",
            )?;
            require(
                volume["type"] == "bind"
                    && volume["bind"]["create_host_path"] == false
                    && !read_only
                    && required_secret(source)
                    && (!source.starts_with("${")
                        || input_identity(source) == "AGENT_CAPABILITY_SANDBOX_CGROUP_DELEGATION"),
                "sandbox cgroup must be its explicitly prepared existing delegation",
            )?;
        }
    }
    require(
        seen == required,
        "sandbox private volume/delegation set incomplete",
    )?;
    let signing_source = control["volumes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|volume| volume_parts(volume).ok())
        .find(|(_, target, _)| *target == "/run/agentforge-sandbox-signing/signing.seed")
        .map(|(source, _, _)| source)
        .ok_or("control purpose signing file missing")?;
    require(
        !signing_source.starts_with("${")
            || input_identity(signing_source) == "AGENT_SANDBOX_SIGNING_KEY_FILE",
        "sandbox signer must use its purpose-specific private input",
    )?;
    let mut control_socket = false;
    for (name, service) in services {
        for volume in service["volumes"].as_array().into_iter().flatten() {
            let (source, target, read_only) = volume_parts(volume)?;
            if name != "agent-capability-worker" {
                require(
                    input_identity(source) != input_identity(signing_source),
                    "sandbox signer source may only be mounted by control",
                )?;
            }
            if name != "agent-python-sandbox"
                && ["sandbox-private-workspaces", "sandbox-private-journal"].contains(&source)
            {
                return Err("sandbox private state shared with another service".into());
            }
            if source == "sandbox-control-socket" {
                require(
                    ["agent-python-sandbox", "agent-capability-worker"].contains(&name.as_str()),
                    "sandbox socket shared outside the control pair",
                )?;
                if name == "agent-capability-worker" {
                    require(
                        target == "/run/agentforge-sandbox" && read_only,
                        "control socket directory must be read-only",
                    )?;
                    control_socket = true;
                }
            }
            if target == "/run/agentforge-sandbox-signing/signing.seed" {
                require(
                    name == "agent-capability-worker",
                    "purpose signing seed may only reach control",
                )?;
            }
            if target.starts_with("/sys/fs/cgroup") {
                require(
                    name == "agent-python-sandbox" && target == "/sys/fs/cgroup/agentforge-sandbox",
                    "only the sandbox receives its delegated cgroup subtree",
                )?;
            }
        }
    }
    require(control_socket, "control private UDS mount missing")?;
    Ok(())
}

pub fn check(config: &Value) -> Result<usize> {
    sandbox_seam(config)?;
    let services = config["services"]
        .as_object()
        .ok_or("Compose services missing")?;
    require(!services.is_empty(), "no container security inputs")?;
    for (name, service) in services {
        // Errors contain service identifiers only, never configuration values.
        let admit = || -> Result<()> {
            require(
                nonroot(&service["user"]),
                "explicit numeric non-root uid:gid required",
            )?;
            require(
                service["read_only"] == true,
                "read-only root filesystem required",
            )?;
            require(
                list(&service["cap_drop"]) == BTreeSet::from(["ALL".to_owned()])
                    && list(&service["cap_add"]).is_empty(),
                "drop ALL capabilities; no additions allowed",
            )?;
            require(
                list(&service["security_opt"]).contains("no-new-privileges:true"),
                "no-new-privileges required",
            )?;
            require(
                !list(&service["security_opt"])
                    .iter()
                    .any(|s| s.ends_with("=unconfined") || s.ends_with(":unconfined")),
                "unconfined security profiles refused",
            )?;
            require(
                service["pids_limit"]
                    .as_u64()
                    .is_some_and(|n| (1..=4096).contains(&n)),
                "finite bounded PID limit required",
            )?;
            require(
                scratch(service),
                "bounded noexec/nosuid/nodev /tmp tmpfs required",
            )?;
            require(
                service["privileged"] != true,
                "privileged containers refused",
            )?;
            for field in ["pid", "ipc", "uts"] {
                require(service[field] != "host", "host namespace refused")?;
            }
            require(
                service.get("devices").is_none() && service.get("device_cgroup_rules").is_none(),
                "host devices refused",
            )?;
            let nets = list(&service["networks"]);
            if ["gateway-init", "agent-python-sandbox"].contains(&name.as_str()) {
                require(
                    service["network_mode"] == "none" && nets.is_empty(),
                    "directory check must have no network",
                )?;
            } else {
                require(
                    service.get("network_mode").is_none(),
                    "shared/default/host network mode refused",
                )?;
                require(!nets.is_empty(), "explicit segmented networks required")?;
            }
            for net in &nets {
                require(
                    owners(net, name),
                    "service is not an owner of its network segment",
                )?;
                require(
                    config["networks"][net].is_object(),
                    "network declaration missing",
                )?;
                if net.starts_with("data") {
                    require(
                        config["networks"][net]["internal"] == true,
                        "data network must be internal",
                    )?;
                }
            }
            for volume in service["volumes"].as_array().into_iter().flatten() {
                let (target, writable, delegated) = if let Some(s) = volume.as_str() {
                    let (head, tail) = s.rsplit_once(':').ok_or("volume target missing")?;
                    if tail == "ro" || tail == "rw" || tail.contains(',') {
                        let (_, target) = head.rsplit_once(':').ok_or("volume target missing")?;
                        (target, !tail.split(',').any(|v| v == "ro"), false)
                    } else {
                        (tail, true, false)
                    }
                } else {
                    (
                        volume["target"].as_str().unwrap_or(""),
                        volume["read_only"] != true,
                        volume["type"] == "bind" && volume["bind"]["create_host_path"] == false,
                    )
                };
                require(
                    !target.contains("docker.sock")
                        && !["/", "/proc", "/sys", "/dev", "/run", "/var/run"].contains(&target),
                    "host control mount refused",
                )?;
                if writable {
                    let allowed = match name.as_str() {
                        "postgres" => target == "/var/lib/postgresql/data",
                        "redis" | "object-store" => target == "/data",
                        "qdrant" => target == "/qdrant/storage",
                        "tempo" => target == "/var/tempo",
                        "gateway" | "gateway-init" => {
                            ["/app/data", "/app/logs", "/app/agent-memory"].contains(&target)
                        }
                        "knowledge-service" | "knowledge-worker" => target == "/app/data",
                        "agent-runtime" => {
                            target == "/var/lib/ai-platform-agent-runtime/runtime-home"
                        }
                        "agent-capability-worker" => target == "/workspace",
                        "agent-python-sandbox" => {
                            [
                                "/workspace",
                                "/run/agentforge-sandbox",
                                "/var/lib/agentforge-sandbox/jobs",
                            ]
                            .contains(&target)
                                || (target == "/sys/fs/cgroup/agentforge-sandbox" && delegated)
                        }
                        "sdk-agent-worker" => target == "/var/lib/sdk-agent",
                        _ => false,
                    };
                    require(allowed, "unreviewed writable mount refused")?;
                }
            }
            if ["frontend", "qdrant", "postgres", "redis", "object-store"].contains(&name.as_str())
            {
                let expected = match name.as_str() {
                    "frontend" => "edge",
                    "qdrant" => "data",
                    "postgres" => "data-postgres",
                    "redis" => "data-redis",
                    _ => "data-object-store",
                };
                require(
                    nets == BTreeSet::from([expected.to_owned()]),
                    "edge/store service may not bridge network segments",
                )?;
            }
            let env = &service["environment"];
            for key in env.as_object().into_iter().flat_map(|m| m.keys()) {
                if key == "QDRANT__SERVICE__API_KEY" || key == "QDRANT__SERVICE__READ_ONLY_API_KEY"
                {
                    require(
                        name == "qdrant",
                        "Qdrant server credential leaked to non-owner",
                    )?;
                }
                if key == "KNOWLEDGE_QDRANT__API_KEY" {
                    require(
                        ["knowledge-service", "knowledge-worker"].contains(&name.as_str()),
                        "Qdrant client credential leaked to non-owner",
                    )?;
                }
            }
            if name == "qdrant" {
                let write = env["QDRANT__SERVICE__API_KEY"].as_str().unwrap_or("");
                let read = env["QDRANT__SERVICE__READ_ONLY_API_KEY"]
                    .as_str()
                    .unwrap_or("");
                require(
                    required_secret(write)
                        && required_secret(read)
                        && input_identity(write) != input_identity(read),
                    "distinct mandatory Qdrant read/write keys required",
                )?;
                require(
                    env["QDRANT__SERVICE__ENABLE_TLS"] == "true"
                        && env["QDRANT__TLS__CERT"] == "/run/qdrant/tls.crt"
                        && env["QDRANT__TLS__KEY"] == "/run/qdrant/tls.key",
                    "Qdrant TLS is mandatory",
                )?;
                for target in [
                    "/run/qdrant/tls.crt",
                    "/run/qdrant/tls.key",
                    "/run/qdrant/ca-bundle.pem",
                ] {
                    require(
                        mount(service, target),
                        "Qdrant TLS inputs require existing read-only file binds",
                    )?;
                }
                let health = service["healthcheck"]["test"].to_string();
                require(
                    health.contains("openssl s_client")
                        && health.contains("-verify_return_error")
                        && health.contains("-verify_hostname qdrant")
                        && !health.contains("-no_check"),
                    "Qdrant health must validate TLS identity",
                )?;
            }
            if ["knowledge-service", "knowledge-worker"].contains(&name.as_str()) {
                require(
                    env["KNOWLEDGE_QDRANT__URL"] == "https://qdrant:6333"
                        && env["KNOWLEDGE_QDRANT__PREFER_GRPC"] == "false",
                    "Knowledge must use verified Qdrant HTTPS",
                )?;
                require(
                    env["SSL_CERT_FILE"] == "/run/qdrant/ca-bundle.pem"
                        && mount(service, "/run/qdrant/ca-bundle.pem"),
                    "Knowledge Qdrant trust bundle missing",
                )?;
                require(
                    !mount(service, "/run/qdrant/tls.key"),
                    "Knowledge may not receive Qdrant server private key",
                )?;
                let role = env["KNOWLEDGE_RUNTIME_ROLE"].as_str().unwrap_or("");
                require(
                    ["api", "worker", "all"].contains(&role),
                    "explicit Knowledge runtime role required",
                )?;
                let key = env["KNOWLEDGE_QDRANT__API_KEY"].as_str().unwrap_or("");
                let expected = if role == "api" {
                    "QDRANT__SERVICE__READ_ONLY_API_KEY"
                } else {
                    "QDRANT__SERVICE__API_KEY"
                };
                let server = config["services"]["qdrant"]["environment"][expected]
                    .as_str()
                    .unwrap_or("");
                require(
                    required_secret(key) && input_identity(key) == input_identity(server),
                    "Knowledge Qdrant credential does not match its role",
                )?;
            }
            if name == "sdk-agent-worker" {
                require(
                    nets == BTreeSet::from(["app".to_owned()]),
                    "SDK worker must have only the app network",
                )?;
                require(
                    env["SDK_AGENT_WORKER_BIND"] == "0.0.0.0:8096",
                    "SDK worker requires its explicit private listener",
                )?;
                let callback = env["SDK_AGENT_GATEWAY_URL"].as_str().unwrap_or("");
                require(
                    [
                        "${SDK_AGENT_GATEWAY_URL:-https://gateway-sdk-callback:8097}",
                        "https://gateway-sdk-callback:8097",
                    ]
                    .contains(&callback)
                        && env["SDK_AGENT_GATEWAY_TLS_CA_FILE"] == "/run/sdk-callback/ca.pem"
                        && mount(service, "/run/sdk-callback/ca.pem"),
                    "SDK app-only callback requires its scoped Gateway HTTPS identity and CA",
                )?;
                for (key, target) in [
                    ("SDK_AGENT_WORKER_TLS_CERT_FILE", "/run/sdk-worker/tls.crt"),
                    ("SDK_AGENT_WORKER_TLS_KEY_FILE", "/run/sdk-worker/tls.key"),
                    ("SDK_AGENT_WORKER_TLS_CA_FILE", "/run/sdk-worker/ca.pem"),
                ] {
                    require(
                        env[key] == target && mount(service, target),
                        "SDK worker TLS file wiring incomplete",
                    )?;
                }
            }
            if name == "gateway" {
                if services.contains_key("sdk-agent-worker") {
                    require(
                        env["SDK_AGENT_GATEWAY_TLS_ENABLED"] == "true"
                            && env["SDK_AGENT_GATEWAY_TLS_BIND"] == "gateway-sdk-callback:8097"
                            && env["SDK_AGENT_GATEWAY_TLS_CERT_FILE"]
                                == "/run/sdk-callback/tls.crt"
                            && env["SDK_AGENT_GATEWAY_TLS_KEY_FILE"] == "/run/sdk-callback/tls.key"
                            && mount(service, "/run/sdk-callback/tls.crt")
                            && mount(service, "/run/sdk-callback/tls.key"),
                        "Gateway SDK callback listener requires app-only bind and separate purpose TLS files",
                    )?;
                    require(
                        service["networks"]["app"]["aliases"]
                            .as_array()
                            .is_some_and(|v| v.iter().any(|a| a == "gateway-sdk-callback")),
                        "Gateway SDK callback alias must belong to app",
                    )?;
                    require(
                        service["networks"].as_object().is_some_and(|networks| {
                            networks.iter().all(|(name, value)| {
                                name == "app"
                                    || !value["aliases"].as_array().is_some_and(|v| {
                                        v.iter().any(|a| a == "gateway-sdk-callback")
                                    })
                            })
                        }),
                        "SDK callback alias must not be assigned to edge or data",
                    )?;
                    require(
                        service["ports"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .all(|port| {
                                if let Some(text) = port.as_str() {
                                    text.rsplit(':')
                                        .next()
                                        .unwrap_or(text)
                                        .trim_end_matches("/tcp")
                                        != "8097"
                                } else {
                                    port["target"] != 8097 && port["target"] != "8097"
                                }
                            }),
                        "Gateway SDK callback port must never be published",
                    )?;
                    require(
                        env["SDK_AGENT_WORKER_URL"] == "https://sdk-agent-worker:8096"
                            && env["SDK_AGENT_WORKER_TLS_CA_FILE"] == "/run/sdk-worker/ca.pem"
                            && mount(service, "/run/sdk-worker/ca.pem"),
                        "Gateway SDK worker transport requires exact private HTTPS identity and CA",
                    )?;
                }
                require(
                    env["GATEWAY_KNOWLEDGE__QDRANT__ENABLED"] == "false"
                        && env.get("GATEWAY_KNOWLEDGE__QDRANT__URL").is_none(),
                    "Gateway must route vectors through Knowledge",
                )?;
            }
            Ok(())
        };
        admit().map_err(|err| format!("container security rejected service {name}: {err}"))?;
    }
    Ok(services.len())
}

fn source(path: &Path) -> Result<Value> {
    let mut yaml: serde_yaml::Value = serde_yaml::from_slice(&std::fs::read(path)?)?;
    yaml.apply_merge()?;
    Ok(serde_json::to_value(yaml)?)
}
fn merge(base: &mut Value, overlay: Value) {
    if let (Some(a), Some(b)) = (base.as_object_mut(), overlay.as_object()) {
        for (key, value) in b {
            if let Some(old) = a.get_mut(key) {
                if ["command", "entrypoint"].contains(&key.as_str()) {
                    *old = value.clone();
                } else {
                    merge(old, value.clone());
                }
            } else {
                a.insert(key.clone(), value.clone());
            }
        }
    } else if let (Some(a), Some(b)) = (base.as_array_mut(), overlay.as_array()) {
        for item in b {
            if !a.contains(item) {
                a.push(item.clone());
            }
        }
    } else {
        *base = overlay;
    }
}
pub fn run(args: &Args) -> Result<Value> {
    match args.positionals.get(1).map(String::as_str) {
        Some("compose") => {
            let config: Value = serde_yaml::from_slice(&std::fs::read(args.get("--config")?)?)
                .map_err(|_| "invalid rendered Compose configuration; values withheld")?;
            Ok(
                json!({"services":check(&config)?,"status":"passed","limit":"configuration admission is not live network or filesystem acceptance"}),
            )
        }
        Some("source") => {
            let root = args.root();
            let base = source(&root.join("docker-compose.yml"))?;
            let mut checked = check(&base)?;
            for file in [
                "docker-compose.full.yml",
                "docker-compose.compact.yml",
                "docker-compose.scale.yml",
                "docker-compose.dev.yml",
                "docker-compose.build.yml",
                "docker-compose.native-candidate.yml",
            ] {
                let mut config = base.clone();
                merge(
                    &mut config,
                    source(&root.join("deploy/compose").join(file))?,
                );
                checked += check(&config)?;
            }
            let local = source(&root.join("deploy/local/compose.yml"))?;
            checked += check(&local)?;
            let mut runtime = local.clone();
            merge(
                &mut runtime,
                source(&root.join("deploy/local/compose.runtime.yml"))?,
            );
            checked += check(&runtime)?;
            let mut packages = runtime.clone();
            merge(
                &mut packages,
                source(&root.join("deploy/compose/docker-compose.sandbox-packages.yml"))?,
            );
            checked += check(&packages)?;
            let mut sandbox = runtime;
            merge(
                &mut sandbox,
                source(&root.join("deploy/local/compose.sandbox.yml"))?,
            );
            checked += check(&sandbox)?;
            checked += check(&source(
                &root.join("deploy/compose/docker-compose.kbms.yml"),
            )?)?;
            let mut sdk = base;
            merge(
                &mut sdk,
                source(&root.join("deploy/compose/docker-compose.sdk-agent-worker.yml"))?,
            );
            checked += check(&sdk)?;
            Ok(
                json!({"status":"passed","service_configurations":checked,"limits":["source interpolation not evaluated","live permissions/TLS/auth/network not tested"]}),
            )
        }
        _ => Err("expected container-security source or compose --config FILE".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..")
    }
    fn fixture() -> Value {
        source(&root().join("docker-compose.yml")).unwrap()
    }
    #[test]
    fn public_source_hardens_all_base_and_local_services() {
        let base = fixture();
        assert_eq!(check(&base).unwrap(), 14);
        for file in [
            "docker-compose.full.yml",
            "docker-compose.compact.yml",
            "docker-compose.scale.yml",
            "docker-compose.dev.yml",
            "docker-compose.build.yml",
            "docker-compose.native-candidate.yml",
        ] {
            let mut composed = base.clone();
            merge(
                &mut composed,
                source(&root().join("deploy/compose").join(file)).unwrap(),
            );
            check(&composed).unwrap();
        }
        let mut packages = base.clone();
        merge(
            &mut packages,
            source(&root().join("deploy/compose/docker-compose.sandbox-packages.yml")).unwrap(),
        );
        check(&packages).unwrap();
        let mut local = source(&root().join("deploy/local/compose.yml")).unwrap();
        assert_eq!(check(&local).unwrap(), 4);
        merge(
            &mut local,
            source(&root().join("deploy/local/compose.runtime.yml")).unwrap(),
        );
        assert_eq!(check(&local).unwrap(), 7);
        merge(
            &mut local,
            source(&root().join("deploy/local/compose.sandbox.yml")).unwrap(),
        );
        check(&local).unwrap();
        check(&source(&root().join("deploy/compose/docker-compose.kbms.yml")).unwrap()).unwrap();
    }
    #[test]
    fn each_container_hardening_control_is_enforced() {
        for (field, value) in [
            ("user", json!("0:0")),
            ("user", json!("root")),
            ("read_only", json!(false)),
            ("cap_drop", json!([])),
            ("cap_add", json!(["SYS_ADMIN"])),
            ("security_opt", json!([])),
            (
                "security_opt",
                json!(["no-new-privileges:true", "seccomp=unconfined"]),
            ),
            ("pids_limit", json!(-1)),
            ("pids_limit", json!(0)),
            ("tmpfs", json!(["/tmp:rw,size=64m"])),
            ("privileged", json!(true)),
            ("pid", json!("host")),
            ("devices", json!(["/dev/kvm"])),
        ] {
            let mut v = fixture();
            v["services"]["gateway"][field] = value;
            assert!(check(&v).is_err(), "control {field} admitted");
        }
    }
    #[test]
    fn edge_and_sandbox_cannot_join_store_or_host_namespaces() {
        for (service, field, value) in [
            ("frontend", "networks", json!(["edge", "data"])),
            (
                "agent-capability-worker",
                "networks",
                json!(["app", "data-postgres", "data"]),
            ),
            ("qdrant", "networks", json!(["data", "app"])),
            ("gateway", "network_mode", json!("host")),
            ("agent-runtime", "network_mode", json!("service:gateway")),
            ("frontend", "networks", json!([])),
        ] {
            let mut v = fixture();
            v["services"][service][field] = value;
            assert!(check(&v).is_err());
        }
        let mut v = fixture();
        v["networks"]["data"]["internal"] = json!(false);
        assert!(check(&v).is_err());
    }
    #[test]
    fn qdrant_missing_shared_optional_wrong_role_and_leaked_keys_are_rejected() {
        for key in [
            "QDRANT__SERVICE__API_KEY",
            "QDRANT__SERVICE__READ_ONLY_API_KEY",
        ] {
            let mut v = fixture();
            v["services"]["qdrant"]["environment"][key] = json!("");
            assert!(check(&v).is_err());
            v["services"]["qdrant"]["environment"][key] = json!("${OPTIONAL_KEY:-}");
            assert!(check(&v).is_err());
        }
        let mut v = fixture();
        v["services"]["qdrant"]["environment"]["QDRANT__SERVICE__READ_ONLY_API_KEY"] =
            v["services"]["qdrant"]["environment"]["QDRANT__SERVICE__API_KEY"].clone();
        assert!(check(&v).is_err());
        let mut v = fixture();
        v["services"]["knowledge-service"]["environment"]["KNOWLEDGE_QDRANT__API_KEY"] =
            json!("${QDRANT_API_KEY:?wrong-role}");
        assert!(check(&v).is_err());
        let mut v = fixture();
        v["services"]["knowledge-worker"]["environment"]["KNOWLEDGE_QDRANT__API_KEY"] =
            json!("${QDRANT_READ_ONLY_API_KEY:?wrong-role}");
        assert!(check(&v).is_err());
        let mut v = fixture();
        v["services"]["frontend"]["environment"]["KNOWLEDGE_QDRANT__API_KEY"] = json!("dummy-leak");
        assert!(check(&v).is_err());
    }
    #[test]
    fn plaintext_unverified_tls_and_missing_trust_are_rejected() {
        for (service, key, value) in [
            ("qdrant", "QDRANT__SERVICE__ENABLE_TLS", "false"),
            (
                "knowledge-service",
                "KNOWLEDGE_QDRANT__URL",
                "http://qdrant:6333",
            ),
            ("knowledge-worker", "SSL_CERT_FILE", ""),
            ("knowledge-service", "KNOWLEDGE_QDRANT__PREFER_GRPC", "true"),
        ] {
            let mut v = fixture();
            v["services"][service]["environment"][key] = json!(value);
            assert!(check(&v).is_err());
        }
        let mut v = fixture();
        v["services"]["qdrant"]["healthcheck"]["test"] =
            json!(["CMD", "curl", "-k", "https://qdrant:6333/healthz"]);
        assert!(check(&v).is_err());
        for field in ["read_only", "create_host_path"] {
            let mut v = fixture();
            let mounts = v["services"]["qdrant"]["volumes"].as_array_mut().unwrap();
            let tls = mounts
                .iter_mut()
                .find(|m| m["target"] == "/run/qdrant/tls.key")
                .unwrap();
            if field == "read_only" {
                tls[field] = json!(false);
            } else {
                tls["bind"][field] = json!(true);
            }
            assert!(check(&v).is_err());
        }
    }
    #[test]
    fn privileged_mount_and_sdk_shared_namespace_are_not_hidden_exceptions() {
        for mount in [
            json!("/var/run/docker.sock:/var/run/docker.sock:ro"),
            json!("host-root:/:ro"),
            json!("unexpected:/etc"),
        ] {
            let mut v = fixture();
            v["services"]["gateway"]["volumes"]
                .as_array_mut()
                .unwrap()
                .push(mount);
            assert!(check(&v).is_err());
        }
        let mut v = fixture();
        merge(
            &mut v,
            source(&root().join("deploy/compose/docker-compose.sdk-agent-worker.yml")).unwrap(),
        );
        assert!(check(&v).is_ok());
        let mut edge_alias = v.clone();
        edge_alias["services"]["gateway"]["networks"]["edge"]["aliases"] =
            json!(["gateway-sdk-callback"]);
        assert!(check(&edge_alias).is_err());
        let mut published = v.clone();
        published["services"]["gateway"]["ports"]
            .as_array_mut()
            .unwrap()
            .push(json!("127.0.0.1:18097:8097/tcp"));
        assert!(check(&published).is_err());
        let mut edge_store = v.clone();
        edge_store["services"]["frontend"]["networks"] = json!(["edge", "data-object-store"]);
        assert!(check(&edge_store).is_err());
        v["services"]["sdk-agent-worker"]["network_mode"] = json!("service:gateway");
        assert!(check(&v).is_err());
    }
    #[test]
    fn sandbox_rejects_credentials_proxy_environment_and_tcp() {
        for (field, value) in [
            ("environment", json!({"AI_PLATFORM_INTERNAL_TOKEN":"dummy"})),
            ("environment", json!({"HTTP_PROXY":"http://proxy.invalid"})),
            ("environment", json!({"DATABASE_URL":"dummy"})),
            ("env_file", json!([".env"])),
            ("secrets", json!(["platform-secret"])),
            ("ports", json!(["8098:8098"])),
            ("expose", json!(["8098"])),
            ("network_mode", json!("host")),
            ("networks", json!(["app"])),
        ] {
            let mut config = fixture();
            config["services"]["agent-python-sandbox"][field] = value;
            assert!(check(&config).is_err(), "sandbox {field} admitted");
        }
    }
    #[test]
    fn sandbox_private_state_signer_and_cgroup_never_reach_other_services() {
        for volume in [
            json!("sandbox-private-workspaces:/borrowed:ro"),
            json!("sandbox-private-journal:/borrowed:ro"),
            json!("sandbox-control-socket:/borrowed:ro"),
            json!({"type":"bind","source":"${AGENT_SANDBOX_SIGNING_KEY_FILE:?do-not-share}","target":"/different-secret-name","read_only":true,"bind":{"create_host_path":false}}),
        ] {
            let mut config = fixture();
            config["services"]["frontend"]["volumes"] = json!([volume]);
            assert!(check(&config).is_err());
        }
        let mut config = fixture();
        config["services"]["agent-python-sandbox"]["volumes"]
            .as_array_mut()
            .unwrap()
            .push(json!("platform-secret:/run/token:ro"));
        assert!(check(&config).is_err());
        let mut config = fixture();
        let delegation = config["services"]["agent-python-sandbox"]["volumes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["target"] == "/sys/fs/cgroup/agentforge-sandbox")
            .unwrap()
            .clone();
        config["services"]["agent-capability-worker"]["volumes"]
            .as_array_mut()
            .unwrap()
            .push(delegation);
        assert!(check(&config).is_err());
    }
    #[test]
    fn sandbox_contract_and_delegation_configuration_fail_closed() {
        for replacement in ["--signing-key-file", "--token", "--bind", "--public-key-id"] {
            let mut config = fixture();
            config["services"]["agent-python-sandbox"]["command"][2] = json!(replacement);
            assert!(check(&config).is_err());
        }
        let mut config = fixture();
        config["services"]["agent-python-sandbox"]["command"][3] = json!("not-a-public-key");
        assert!(check(&config).is_err());
        let mut config = fixture();
        config["services"]["agent-python-sandbox"]["image"] =
            config["services"]["agent-capability-worker"]["image"].clone();
        assert!(check(&config).is_err());
        let mut config = fixture();
        config["services"]["agent-python-sandbox"]["security_opt"] =
            json!(["no-new-privileges:true"]);
        assert!(check(&config).is_err());
        for bad_source in [
            "/sys/fs/cgroup",
            "/",
            "relative-cgroup",
            "/sys/fs/cgroup/../cgroup",
        ] {
            let mut config = fixture();
            let delegation = config["services"]["agent-python-sandbox"]["volumes"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|v| v["target"] == "/sys/fs/cgroup/agentforge-sandbox")
                .unwrap();
            delegation["source"] = json!(bad_source);
            assert!(check(&config).is_err());
        }
        for field in ["source", "target", "create_host_path"] {
            let mut config = fixture();
            let delegation = config["services"]["agent-python-sandbox"]["volumes"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|v| v["target"] == "/sys/fs/cgroup/agentforge-sandbox")
                .unwrap();
            match field {
                "source" => {
                    delegation[field] =
                        json!("${AGENT_CAPABILITY_WORKER_CGROUP_DELEGATION:?wrong-owner}")
                }
                "target" => delegation[field] = json!("/sys/fs/cgroup"),
                _ => delegation["bind"][field] = json!(true),
            };
            assert!(check(&config).is_err());
        }
    }
    #[test]
    fn control_uds_mount_owner_health_and_purpose_key_selection_are_enforced() {
        let mut config = fixture();
        config["services"]
            .as_object_mut()
            .unwrap()
            .remove("agent-python-sandbox");
        assert!(check(&config).is_err());
        let mut config = fixture();
        let socket = config["services"]["agent-capability-worker"]["volumes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|v| {
                v.as_str()
                    .is_some_and(|v| v.starts_with("sandbox-control-socket:"))
            })
            .unwrap();
        *socket = json!("sandbox-control-socket:/run/agentforge-sandbox");
        assert!(check(&config).is_err());
        let mut config = fixture();
        config["services"]["agent-capability-worker"]["environment"]["AI_PLATFORM_SANDBOX_KEY_ID"] =
            json!("wrong-purpose-key");
        assert!(check(&config).is_err());
        let mut config = fixture();
        config["services"]["agent-python-sandbox"]["user"] = json!("1000:1000");
        assert!(check(&config).is_err());
        let mut config = fixture();
        config["services"]["agent-capability-worker"]["depends_on"]["agent-python-sandbox"]["condition"] =
            json!("service_started");
        assert!(check(&config).is_err());
        let mut config = fixture();
        config["services"]["agent-python-sandbox"]["healthcheck"]["test"] =
            json!(["CMD", "curl", "http://localhost/health"]);
        assert!(check(&config).is_err());
    }
}
