//! Offline rejection fixtures call the production validators used by the gates.
use super::*;
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        let p = std::env::temp_dir().join(format!("agentforge-pe40-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p)?;
        Ok(Self(p))
    }
    fn put(&self, name: &str, text: &str) -> Result<()> {
        let path = self.0.join(name);
        std::fs::create_dir_all(path.parent().ok_or("fixture parent missing")?)?;
        std::fs::write(path, text)?;
        Ok(())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn case(
    out: &mut Vec<Value>,
    findings: &[&str],
    gate: &str,
    name: &str,
    accept: bool,
    outcome: Result<()>,
) -> Result<()> {
    let error = outcome.as_ref().err().map(ToString::to_string);
    require(
        error
            .as_ref()
            .is_none_or(|s| !s.contains("NEVER_LOG_FIXTURE_SECRET")),
        "recurrence validator leaked credential value",
    )?;
    require(
        outcome.is_ok() == accept,
        format!("recurrence fixture {name} did not preserve the expected gate decision"),
    )?;
    out.push(json!({"case":name,"findings":findings,"original_gate":gate,"expected":if accept{"accept"}else{"reject"},
        "result":if accept{"pass"}else{"rejected_as_required"}}));
    Ok(())
}
pub(super) fn run() -> Result<Value> {
    let mut out = crate::compose_identity::recurrence_selftest()?
        .as_array()
        .cloned()
        .ok_or("image recurrence cases missing")?;
    for value in &mut out {
        value["original_gate"] = json!("compose-image-identity/source-lock-pairing");
    }
    let good = json!({"services":{"gateway":{"environment":{"DATABASE_URL":"postgresql://ai_gateway_gateway:NEVER_LOG_FIXTURE_SECRET@postgres/db"}}}});
    case(
        &mut out,
        &["F-05"],
        "database-identity",
        "scoped_gateway_identity",
        true,
        crate::database_identity::check_compose(&good, "ai_gateway_").map(|_| ()),
    )?;
    for user in ["postgres", "root", "ai_gateway_knowledge_worker"] {
        let mut bad = good.clone();
        bad["services"]["gateway"]["environment"]["DATABASE_URL"] = json!(format!(
            "postgresql://{user}:NEVER_LOG_FIXTURE_SECRET@postgres/db"
        ));
        case(
            &mut out,
            &["F-05"],
            "database-identity",
            &format!("forbidden_database_role_{user}"),
            false,
            crate::database_identity::check_compose(&bad, "ai_gateway_").map(|_| ()),
        )?;
    }
    let mut bad = good.clone();
    bad["services"]["gateway"]["environment"]["AI_GATEWAY_DATABASE_ADMIN_DSN"] =
        json!("NEVER_LOG_FIXTURE_SECRET");
    case(
        &mut out,
        &["F-05"],
        "database-identity",
        "admin_dsn_alias",
        false,
        crate::database_identity::check_compose(&bad, "ai_gateway_").map(|_| ()),
    )?;
    let source = json!({"services":{"gateway":{"environment":{"DATABASE_URL":"postgresql://${AI_GATEWAY_ROLE_PREFIX:-ai_gateway_}gateway:${LOCAL_DB_GATEWAY_PASSWORD:?required}@postgres/db"}}}});
    case(
        &mut out,
        &["F-05"],
        "database-identity",
        "public_scoped_source_expression",
        true,
        crate::database_identity::check_source(&source).map(|_| ()),
    )?;
    let mut bad = source;
    bad["services"]["gateway"]["environment"]["DATABASE_URL"] =
        json!("postgresql://${POSTGRES_USER:-postgres}:${POSTGRES_PASSWORD:?required}@postgres/db");
    case(
        &mut out,
        &["F-05"],
        "database-identity",
        "bootstrap_source_expression",
        false,
        crate::database_identity::check_source(&bad).map(|_| ()),
    )?;

    let workflow_good = json!({"permissions":{"contents":"read"},"jobs":{"test":{"steps":[{"uses":format!("actions/checkout@{}","a".repeat(40))}]}}});
    case(
        &mut out,
        &["F-23"],
        "platform-fitness/workflow-sha-and-permissions",
        "immutable_action_and_permissions",
        true,
        workflow(&workflow_good),
    )?;
    for (name, pointer, value) in [
        (
            "tag_pinned_action",
            "/jobs/test/steps/0/uses",
            json!("actions/checkout@v4"),
        ),
        (
            "short_action_sha",
            "/jobs/test/steps/0/uses",
            json!("actions/checkout@abcd1234"),
        ),
        ("missing_workflow_permissions", "/permissions", Value::Null),
        (
            "broad_workflow_permissions",
            "/permissions",
            json!("write-all"),
        ),
        (
            "broad_job_permissions",
            "/jobs/test/permissions",
            json!("write-all"),
        ),
        (
            "local_action_traversal",
            "/jobs/test/steps/0/uses",
            json!("./../foreign/action"),
        ),
        (
            "invalid_local_prefix",
            "/jobs/test/steps/0/uses",
            json!("$/.github/actions/setup-native"),
        ),
    ] {
        let mut bad = workflow_good.clone();
        if pointer == "/jobs/test/permissions" {
            bad["jobs"]["test"]["permissions"] = value;
        } else {
            *bad.pointer_mut(pointer)
                .ok_or("workflow fixture pointer missing")? = value;
        }
        case(
            &mut out,
            &["F-23"],
            "platform-fitness/workflow-sha-and-permissions",
            name,
            false,
            workflow(&bad),
        )?;
    }
    let composite =
        json!({"permissions":{},"jobs":{"composite":{"steps":[{"uses":"vendor/action@mutable"}]}}});
    case(
        &mut out,
        &["F-23"],
        "platform-fitness/workflow-sha-and-permissions",
        "nested_composite_action_tag",
        false,
        workflow(&composite),
    )?;

    let store = json!({"services":{"redis":{"command":["redis-server","--maxmemory-policy","noeviction"]},"qdrant":{"environment":{"QDRANT__SERVICE__API_KEY":"${QDRANT_API_KEY:?required}"}}}});
    case(
        &mut out,
        &["F-39", "F-40"],
        "valkey-deployment/platform-fitness-coordination",
        "bounded_coordination_and_vector_auth",
        true,
        coordination(&store),
    )?;
    for (name, command) in [
        (
            "evictable_coordination",
            json!(["redis-server", "--maxmemory-policy", "allkeys-lru"]),
        ),
        (
            "duplicate_last_policy_wins",
            json!([
                "redis-server",
                "--maxmemory-policy",
                "noeviction",
                "--maxmemory-policy",
                "allkeys-lru"
            ]),
        ),
        (
            "unrelated_noeviction_token",
            json!([
                "redis-server",
                "--maxmemory-policy",
                "allkeys-lru",
                "--name",
                "noeviction"
            ]),
        ),
        (
            "shell_policy_spoof",
            json!("echo noeviction; redis-server --maxmemory-policy allkeys-lru"),
        ),
    ] {
        let mut bad = store.clone();
        bad["services"]["redis"]["command"] = command;
        case(
            &mut out,
            &["F-40"],
            "platform-fitness-coordination",
            name,
            false,
            coordination(&bad),
        )?;
    }
    for (name, key) in [
        ("missing_qdrant_auth", Value::Null),
        ("empty_qdrant_auth", json!("")),
        ("optional_qdrant_auth", json!("${QDRANT_API_KEY:-}")),
    ] {
        let mut bad = store.clone();
        bad["services"]["qdrant"]["environment"]["QDRANT__SERVICE__API_KEY"] = key;
        case(
            &mut out,
            &["F-39"],
            "container-security/platform-fitness-qdrant",
            name,
            false,
            coordination(&bad),
        )?;
    }
    let scratch = Scratch::new()?;
    let mut wrapped = store;
    wrapped["services"]["redis"]["command"] = json!(["/bin/sh", "/entry.sh"]);
    wrapped["services"]["redis"]["volumes"] = json!(["./entry.sh:/entry.sh:ro"]);
    scratch.put(
        "entry.sh",
        "#!/bin/sh\nruntime_config=/tmp/redis.conf\n{\nprintf '%s\\n' 'maxmemory-policy noeviction'\n} > \"$runtime_config\"\nexec /usr/local/bin/docker-entrypoint.sh redis-server \"$runtime_config\"\n",
    )?;
    case(
        &mut out,
        &["F-40"],
        "platform-fitness-coordination",
        "resolved_readonly_coordination_wrapper",
        true,
        coordination_source(&scratch.0, &wrapped),
    )?;
    for (name, script) in [
        (
            "wrapper_unused_policy_spoof",
            "printf '%s\\n' 'maxmemory-policy noeviction'\nexec redis-server\n",
        ),
        (
            "wrapper_comment_spoof",
            "# maxmemory-policy noeviction\nprintf '%s\\n' 'maxmemory-policy allkeys-lru'\n",
        ),
        (
            "wrapper_duplicate_policy",
            "printf '%s\\n' 'maxmemory-policy noeviction'\nprintf '%s\\n' 'maxmemory-policy allkeys-lru'\n",
        ),
    ] {
        scratch.put("entry.sh", script)?;
        case(
            &mut out,
            &["F-40"],
            "platform-fitness-coordination",
            name,
            false,
            coordination_source(&scratch.0, &wrapped),
        )?;
    }

    let hard = json!({"services":{"frontend":{"user":"101:101","read_only":true,"cap_drop":["ALL"],"security_opt":["no-new-privileges:true"],"pids_limit":128,"tmpfs":["/tmp:rw,noexec,nosuid,nodev,size=16m"],"networks":["edge"]}},"networks":{"edge":{},"data":{"internal":true}}});
    case(
        &mut out,
        &["F-36"],
        "container-security",
        "hardened_frontend",
        true,
        crate::container_security::check(&hard).map(|_| ()),
    )?;
    for key in [
        "user",
        "read_only",
        "cap_drop",
        "security_opt",
        "pids_limit",
        "tmpfs",
    ] {
        let mut bad = hard.clone();
        bad["services"]["frontend"]
            .as_object_mut()
            .unwrap()
            .remove(key);
        case(
            &mut out,
            &["F-36"],
            "container-security",
            &format!("missing_hardening_{key}"),
            false,
            crate::container_security::check(&bad).map(|_| ()),
        )?;
    }
    let mut bad = hard;
    bad["services"]["frontend"]["networks"] = json!(["edge", "data"]);
    case(
        &mut out,
        &["F-39"],
        "container-security",
        "edge_reaches_vector_segment",
        false,
        crate::container_security::check(&bad).map(|_| ()),
    )?;

    let clear = json!({"services":{"knowledge-service":{"environment":{"KNOWLEDGE_QDRANT__API_KEY":"scoped-vector-key","KNOWLEDGE_MODEL_PLANE__WORKLOAD_KEY_FILE":"/run/model/signing.json"}},"sdk-agent-worker":{"environment":{"SDK_AGENT_WORKER_TOKEN":"scoped-platform-transport"}}}});
    case(
        &mut out,
        &["F-11", "F-33"],
        "platform-fitness/provider-credential-boundary",
        "purpose_scoped_nonprovider_keys",
        true,
        provider_boundary(&clear),
    )?;
    for key in [
        "OPENAI_API_KEY",
        "DASHSCOPE_API_KEY",
        "DEEPSEEK_API_KEY",
        "GEMINI_API_KEY_FILE",
        "GATEWAY_PROVIDERS__CONFIG",
        "GOOGLE_APPLICATION_CREDENTIALS",
    ] {
        for role in ["knowledge-service", "sdk-agent-worker"] {
            let mut bad = clear.clone();
            bad["services"][role]["environment"][key] = json!("NEVER_LOG_FIXTURE_SECRET");
            case(
                &mut out,
                &["F-11", "F-33"],
                "platform-fitness/provider-credential-boundary",
                &format!("{role}_{key}"),
                false,
                provider_boundary(&bad),
            )?;
        }
    }
    let mut bad = clear.clone();
    bad["services"]["knowledge-service"]["env_file"] = json!(["/private/opaque.env"]);
    case(
        &mut out,
        &["F-33"],
        "platform-fitness/provider-credential-boundary",
        "opaque_provider_environment_file",
        false,
        provider_boundary(&bad),
    )?;
    let mut bad = clear.clone();
    bad["services"]["sdk-agent-worker"]["environment"] = json!(["OPENAI_API_KEY"]);
    case(
        &mut out,
        &["F-11"],
        "platform-fitness/provider-credential-boundary",
        "ambient_list_environment_credential",
        false,
        provider_boundary(&bad),
    )?;
    let mut bad = clear;
    bad["services"]["sdk-agent-worker"]["secrets"] =
        json!([{"source":"provider-api-key","target":"opaque"}]);
    case(
        &mut out,
        &["F-11"],
        "platform-fitness/provider-credential-boundary",
        "provider_secret_reference",
        false,
        provider_boundary(&bad),
    )?;

    scratch.put(
        "docker-compose.yml",
        "services:\n  knowledge-service:\n    environment: {}\n",
    )?;
    scratch.put(
        "deploy/compose/compose.sdk.yml",
        "services:\n  sdk-agent-worker:\n    environment: {}\n",
    )?;
    case(
        &mut out,
        &["F-11", "F-33"],
        "platform-fitness/provider-credential-boundary",
        "public_overlay_tree_without_credentials",
        true,
        provider_sources(&scratch.0),
    )?;
    scratch.put("deploy/compose/compose.sdk.yml", "services:\n  sdk-agent-worker:\n    environment:\n      DASHSCOPE_API_KEY: NEVER_LOG_FIXTURE_SECRET\n")?;
    case(
        &mut out,
        &["F-11", "F-33"],
        "platform-fitness/provider-credential-boundary",
        "sdk_overlay_provider_key",
        false,
        provider_sources(&scratch.0),
    )?;
    for (name, text) in [
        (
            "renamed_gateway_dependency",
            "[package]\nname='ai-platform-knowledge-domain'\n[dependencies]\ninnocent={package='ai-platform-gateway',version='1'}",
        ),
        (
            "target_specific_gateway_dependency",
            "[package]\nname='ai-platform-knowledge-domain'\n[target.'cfg(unix)'.build-dependencies]\ninnocent={package='ai-platform-gateway',version='1'}",
        ),
        (
            "renamed_contract_io",
            "[package]\nname='ai-platform-contracts'\n[dev-dependencies]\ninnocent={package='reqwest',version='1'}",
        ),
    ] {
        case(
            &mut out,
            &["F-15"],
            "architecture-boundary/platform-fitness-dependency",
            name,
            false,
            dependency_manifest(&toml::from_str(text)?),
        )?;
    }
    scratch.put(
        "Cargo.toml",
        "[workspace]\n[workspace.dependencies]\ninnocent={package='serde',version='1'}\n",
    )?;
    scratch.put("knowledge/Cargo.toml", "[package]\nname='ai-platform-knowledge-domain'\n[target.'cfg(unix)'.dependencies]\ninnocent.workspace=true\n")?;
    case(
        &mut out,
        &["F-15"],
        "architecture-boundary/platform-fitness-dependency",
        "resolved_pure_workspace_alias",
        true,
        resolved_dependency_manifest(&scratch.0, Path::new("knowledge/Cargo.toml")),
    )?;
    scratch.put("Cargo.toml","[workspace]\n[workspace.dependencies]\ninnocent={package='ai-platform-gateway',path='gateway'}\n")?;
    scratch.put(
        "gateway/Cargo.toml",
        "[package]\nname='ai-platform-gateway'\nversion='1.0.0'\n",
    )?;
    scratch.put("knowledge/Cargo.toml","[package]\nname='ai-platform-knowledge-domain'\n[target.'cfg(unix)'.dependencies]\ninnocent.workspace=true\n")?;
    case(
        &mut out,
        &["F-15"],
        "architecture-boundary/platform-fitness-dependency",
        "inherited_target_alias_gateway",
        false,
        resolved_dependency_manifest(&scratch.0, Path::new("knowledge/Cargo.toml")),
    )?;
    scratch.put("knowledge/Cargo.toml","[package]\nname='ai-platform-knowledge-domain'\n[dependencies]\ninnocent={path='../gateway'}\n")?;
    case(
        &mut out,
        &["F-15"],
        "architecture-boundary/platform-fitness-dependency",
        "path_only_alias_gateway",
        false,
        resolved_dependency_manifest(&scratch.0, Path::new("knowledge/Cargo.toml")),
    )?;
    case(
        &mut out,
        &["F-15"],
        "architecture-boundary/platform-fitness-dependency",
        "pure_contract_serde",
        true,
        dependency_manifest(&toml::from_str(
            "[package]\nname='ai-platform-contracts'\n[dependencies]\nserde='1'\n",
        )?),
    )?;

    case(
        &mut out,
        &["F-22", "F-37"],
        "platform-fitness/supported-production-bases",
        "supported_multistage_defaults",
        true,
        base_images(
            "ARG BASE=debian:trixie-slim\nFROM ${BASE} AS base\nFROM base AS runtime\nFROM nginxinc/nginx-unprivileged:1.30-alpine AS web\nFROM python:3.14-slim-trixie AS sandbox\n",
        ),
    )?;
    for image in [
        "nginx:1.27-alpine",
        "nginxinc/nginx-unprivileged:1.27-alpine",
        "nginx:1.28",
        "python:2.7",
        "python:3.10-slim",
        "debian:11-slim",
        "debian:bullseye",
        "python:3.14-slim-bullseye",
        "vendor/tool:latest",
        "nginx",
    ] {
        case(
            &mut out,
            &["F-22", "F-37"],
            "platform-fitness/supported-production-bases",
            &format!("unsupported_base_{image}"),
            false,
            base_images(&format!("FROM {image}\n")),
        )?;
    }
    case(
        &mut out,
        &["F-22", "F-37"],
        "platform-fitness/supported-production-bases",
        "nested_unsupported_base_default",
        false,
        base_images(
            "ARG OLD=nginxinc/nginx-unprivileged:1.27-alpine\nARG BASE=${OLD}\nFROM ${BASE}\n",
        ),
    )?;
    case(
        &mut out,
        &["F-22", "F-37"],
        "platform-fitness/supported-production-bases",
        "stage_arg_cannot_override_global_from",
        false,
        base_images(
            "ARG BASE=nginx:1.27-alpine\nFROM debian:trixie-slim AS builder\nARG BASE=nginx:1.30-alpine\nFROM ${BASE}\n",
        ),
    )?;
    let findings: BTreeSet<_> = out
        .iter()
        .flat_map(|c| c["findings"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    require(
        findings.len() == 12,
        "recurrence finding coverage incomplete",
    )?;
    for finding in &findings {
        require(
            out.iter().any(|v| {
                v["findings"]
                    .as_array()
                    .is_some_and(|a| a.contains(&json!(finding)))
                    && v["result"] == "rejected_as_required"
            }),
            "recurrence finding has no rejection fixture",
        )?;
    }
    Ok(json!({"result":"pass","cases":out,"findings":findings,"runtime_acceptance":false}))
}
