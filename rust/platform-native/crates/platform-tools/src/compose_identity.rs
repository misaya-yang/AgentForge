//! Configuration identity only. Runnable image admission remains source-lock verify-local-images.
use crate::*;
use std::collections::BTreeSet;

const UNITS: &[(&str, &str)] = &[
    ("gateway", "GATEWAY_IMAGE"),
    ("frontend", "FRONTEND_IMAGE"),
    ("knowledge-service", "KNOWLEDGE_IMAGE"),
    ("knowledge-worker", "KNOWLEDGE_IMAGE"),
    ("migrate", "MIGRATE_IMAGE"),
    ("agent-runtime", "AI_PLATFORM_AGENT_RUNTIME_IMAGE"),
    ("agent-capability-worker", "AGENT_CAPABILITY_WORKER_IMAGE"),
];

pub fn source_contract(root: &Path) -> Result<()> {
    let base: Value = serde_yaml::from_slice(&std::fs::read(root.join("docker-compose.yml"))?)?;
    source_value_contract(&base)
}
fn source_value_contract(base: &Value) -> Result<()> {
    let mut defaults = BTreeSet::new();
    for (service, variable) in UNITS {
        let unit = &base["services"][service];
        require(
            unit.is_object(),
            format!("Compose missing service: {service}"),
        )?;
        require(
            unit.get("build").is_none(),
            "Base Compose must use images; builds belong in the opt-in overlay",
        )?;
        let expression = unit["image"].as_str().unwrap_or("");
        require(
            expression.starts_with(&format!("${{{variable}:")),
            format!("Compose {service} must use its public {variable} override"),
        )?;
        require(
            expression.ends_with('}') && !expression[2..].contains("${"),
            "image override must not alias another code unit",
        )?;
        if let Some(default) = expression
            .split_once(":-")
            .map(|(_, v)| v.trim_end_matches('}'))
        {
            require(
                !default.is_empty() && (*service == "knowledge-worker" || defaults.insert(default)),
                "different code units share an image default",
            )?;
        }
        if ["agent-runtime", "agent-capability-worker"].contains(service) {
            require(
                expression.starts_with(&format!("${{{variable}:?")),
                "Runtime/Worker require explicit current paired source-lock selections",
            )?;
        }
    }
    require(
        base["services"]["knowledge-service"]["image"]
            == base["services"]["knowledge-worker"]["image"],
        "Knowledge API and worker image defaults differ",
    )
}

pub fn check(root: &Path, config: &Value, mode: &str, source_build: bool) -> Result<Value> {
    source_contract(root)?;
    require(
        ["compact", "full", "scale"].contains(&mode),
        "Unknown Compose topology mode",
    )?;
    let services = config["services"]
        .as_object()
        .ok_or("Rendered Compose services missing")?;
    let mut images = BTreeSet::new();
    for (service, _) in UNITS {
        if *service == "knowledge-worker" && mode == "compact" && !services.contains_key(*service) {
            continue;
        }
        let unit = services
            .get(*service)
            .ok_or_else(|| format!("Rendered Compose missing service: {service}"))?;
        let image = unit["image"]
            .as_str()
            .filter(|s| !s.is_empty() && !s.contains("${"))
            .ok_or("Rendered Compose image is missing or unresolved")?;
        if *service != "knowledge-worker" {
            require(
                images.insert(image),
                "Different code units resolve to the same image",
            )?;
        }
        if !source_build || matches!(*service, "agent-runtime" | "agent-capability-worker") {
            require(
                unit.get("build").is_none(),
                "Unexpected source build; Runtime/Worker require their controlled source workflow",
            )?;
        }
    }
    if let Some(worker) = services.get("knowledge-worker") {
        require(
            worker["image"] == services["knowledge-service"]["image"],
            "Knowledge API and worker resolve to different images",
        )?;
    }
    if source_build {
        for (service, context, dockerfile, target) in [
            (
                "gateway",
                ".",
                "deploy/platform-native/Dockerfile",
                Some("runtime"),
            ),
            ("frontend", "web", "Dockerfile", None),
            (
                "knowledge-service",
                ".",
                "deploy/platform-native/Dockerfile",
                Some("knowledge"),
            ),
            (
                "knowledge-worker",
                ".",
                "deploy/platform-native/Dockerfile",
                Some("knowledge"),
            ),
            (
                "migrate",
                ".",
                "deploy/platform-native/Dockerfile",
                Some("migrate"),
            ),
        ] {
            let Some(unit) = services.get(service) else {
                continue;
            };
            let build = &unit["build"];
            let expected_context = root.join(context).canonicalize()?;
            require(
                build["context"].as_str().is_some_and(|p| {
                    Path::new(p).canonicalize().ok().as_ref() == Some(&expected_context)
                }) && build["dockerfile"] == dockerfile
                    && match target {
                        Some(t) => build["target"] == t,
                        None => build.get("target").is_none_or(|t| t.is_null() || t == ""),
                    },
                format!("Source build does not match the {service} code unit"),
            )?;
        }
    }
    let lock = object(&root.join("deploy/agent-runtime-source/lock.json"))?;
    pairing(services, &lock)?;
    Ok(
        json!({"result":"pass","mode":mode,"source_build":source_build,
        "identity":"configuration_matches_source_lock","artifact_acceptance":"not_checked; source-lock verify-local-images remains required"}),
    )
}

fn pairing(services: &serde_json::Map<String, Value>, lock: &Value) -> Result<()> {
    let upstream = lock["source"]["upstream_sha"]
        .as_str()
        .ok_or("Source lock upstream missing")?;
    let overlay = lock["build"]["overlay_sha256"]
        .as_str()
        .ok_or("Source lock overlay missing")?;
    require(
        upstream.len() == 40
            && overlay.len() == 64
            && upstream
                .bytes()
                .chain(overlay.bytes())
                .all(|b| b.is_ascii_hexdigit()),
        "Source lock identity malformed",
    )?;
    for (service, artifact, prefix) in [
        ("agent-runtime", "agent_runtime", "ai-gateway-agent-runtime"),
        (
            "agent-capability-worker",
            "capability_worker",
            "ai-gateway-agent-capability-worker",
        ),
    ] {
        let image = services[service]["image"].as_str().unwrap();
        let expected = format!("{prefix}:local-{}-{}", &upstream[..12], &overlay[..12]);
        let artifact = &lock["oci"]["artifacts"][artifact];
        let recorded_ref = artifact["image_ref"]
            .as_str()
            .filter(|r| !r.starts_with("local-image://"));
        let digest = artifact["image_digest"].as_str();
        let recorded = artifact["candidate_start_allowed"] == true
            && (recorded_ref == Some(image)
                || digest == Some(image)
                || image
                    .split_once('@')
                    .is_some_and(|(_, d)| digest == Some(d)));
        require(
            image == expected || recorded,
            format!("{service} image selection differs from the current source lock"),
        )?;
    }
    let revision = format!("{upstream}+{}", &overlay[..12]);
    require(
        services["agent-runtime"]["environment"]["AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION"]
            == revision,
        "Runtime kernel revision differs from the current source lock",
    )?;
    Ok(())
}
pub(crate) fn recurrence_selftest() -> Result<Value> {
    let mut source = json!({"services":{}});
    for (service, variable) in UNITS {
        source["services"][*service] = json!({"image":if ["agent-runtime","agent-capability-worker"].contains(service) {
            format!("${{{variable}:?select paired image}}")
        } else { format!("${{{variable}:-example.invalid/{variable}:2}}") }});
    }
    source_value_contract(&source)?;
    let mut cases =
        vec![json!({"case":"distinct_code_units","findings":["F-01","F-02"],"result":"pass"})];
    for (name, service, expression) in [
        (
            "web_knowledge_variable",
            "frontend",
            "${KNOWLEDGE_IMAGE:-example.invalid/KNOWLEDGE_IMAGE:2}",
        ),
        (
            "web_knowledge_default_collision",
            "frontend",
            "${FRONTEND_IMAGE:-example.invalid/KNOWLEDGE_IMAGE:2}",
        ),
        (
            "nested_web_knowledge_alias",
            "frontend",
            "${FRONTEND_IMAGE:-${KNOWLEDGE_IMAGE:-collided}}",
        ),
        (
            "mismatched_knowledge_worker",
            "knowledge-worker",
            "${KNOWLEDGE_IMAGE:-different:2}",
        ),
        (
            "stale_runtime_default",
            "agent-runtime",
            "${AI_PLATFORM_AGENT_RUNTIME_IMAGE:-old-runtime:1}",
        ),
    ] {
        let mut bad = source.clone();
        bad["services"][service]["image"] = json!(expression);
        require(
            source_value_contract(&bad).is_err(),
            "image recurrence negative unexpectedly accepted",
        )?;
        cases.push(json!({"case":name,"findings":["F-01","F-02"],"result":"rejected_as_required"}));
    }
    let upstream = "a".repeat(40);
    let overlay = "b".repeat(64);
    let lock = json!({"source":{"upstream_sha":upstream},"build":{"overlay_sha256":overlay}});
    let mut services = json!({"agent-runtime":{"image":format!("ai-gateway-agent-runtime:local-{}-{}",&upstream[..12],&overlay[..12]),
        "environment":{"AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION":format!("{upstream}+{}",&overlay[..12])}},
        "agent-capability-worker":{"image":format!("ai-gateway-agent-capability-worker:local-{}-{}",&upstream[..12],&overlay[..12])}});
    pairing(services.as_object().unwrap(), &lock)?;
    for name in ["agent-runtime", "agent-capability-worker"] {
        let old = services[name]["image"].clone();
        services[name]["image"] = json!("stale:1");
        require(
            pairing(services.as_object().unwrap(), &lock).is_err(),
            "stale paired image was accepted",
        )?;
        cases.push(json!({"case":format!("stale_{name}"),"findings":["F-02"],"result":"rejected_as_required"}));
        services[name]["image"] = old;
    }
    services["agent-runtime"]["environment"]["AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION"] =
        json!("old");
    require(
        pairing(services.as_object().unwrap(), &lock).is_err(),
        "stale kernel revision was accepted",
    )?;
    cases.push(
        json!({"case":"stale_kernel_revision","findings":["F-02"],"result":"rejected_as_required"}),
    );
    Ok(json!(cases))
}

pub fn run(args: &Args) -> Result<Value> {
    check(
        &args.root(),
        &json_input(args.get("--config")?)?,
        args.values
            .get("--mode")
            .map(String::as_str)
            .unwrap_or("full"),
        args.flag("--source-build"),
    )
}

/// Real Compose interpolation/merge matrix, using public examples only. Never starts Docker services.
pub fn matrix(root: &Path) -> Result<Value> {
    let root = root.canonicalize()?;
    let overrides = [
        (
            "GATEWAY_IMAGE",
            "gateway",
            "example.invalid/upgrade/gateway:2.0.0",
        ),
        (
            "FRONTEND_IMAGE",
            "frontend",
            "example.invalid/upgrade/web:2.0.0",
        ),
        (
            "KNOWLEDGE_IMAGE",
            "knowledge-service",
            "example.invalid/upgrade/knowledge:2.0.0",
        ),
        (
            "MIGRATE_IMAGE",
            "migrate",
            "example.invalid/upgrade/migrate:2.0.0",
        ),
    ];
    let render = |mode: &str, variant: &str, negative: Option<(&str, &str)>| -> Result<Value> {
        let mut command = std::process::Command::new("docker");
        command.current_dir(&root).env_clear();
        // Keep only executable/plugin discovery. Ambient credentials and Compose overrides must
        // neither enter this public-config gate nor change its test population.
        for key in ["PATH", "HOME", "DOCKER_CONFIG"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        // Render-only fixture credentials. Public examples deliberately leave
        // these unset so real deployments must generate distinct passwords.
        // Nothing from the selected private env or ambient credentials is used.
        for principal in [
            "MIGRATOR",
            "GATEWAY",
            "RUNTIME",
            "CAPABILITY_WORKER",
            "KNOWLEDGE_API",
            "KNOWLEDGE_WORKER",
        ] {
            command.env(
                format!("LOCAL_DB_{principal}_PASSWORD"),
                format!("public-render-fixture-{principal}"),
            );
        }
        // Optional profiles are interpolated by Compose even when not started.
        // These synthetic references are render-only and deliberately unusable.
        for (key, value) in [
            ("QDRANT_API_KEY", "public-render-writer-fixture"),
            ("QDRANT_READ_ONLY_API_KEY", "public-render-reader-fixture"),
            ("QDRANT_TLS_CERT_FILE", "/public-render-fixture/qdrant.crt"),
            ("QDRANT_TLS_KEY_FILE", "/public-render-fixture/qdrant.key"),
            ("QDRANT_CA_BUNDLE_FILE", "/public-render-fixture/ca.pem"),
            (
                "AGENT_PYTHON_SANDBOX_IMAGE",
                "example.invalid/render/sandbox:unrunnable",
            ),
            ("AGENT_SANDBOX_PUBLIC_KEY_ID", "public-render-fixture"),
            (
                "AGENT_SANDBOX_PUBLIC_KEY_HEX",
                "1111111111111111111111111111111111111111111111111111111111111111",
            ),
            (
                "AGENT_SANDBOX_SIGNING_KEY_FILE",
                "/public-render-fixture/sandbox.seed",
            ),
            (
                "AGENT_CAPABILITY_SANDBOX_CGROUP_DELEGATION",
                "/public-render-fixture/cgroup",
            ),
        ] {
            command.env(key, value);
        }
        command
            .args([
                "compose",
                "--env-file",
                ".env.example",
                "-f",
                "docker-compose.yml",
                "-f",
            ])
            .arg(format!("deploy/compose/docker-compose.{mode}.yml"));
        if variant == "source_build" {
            command.args(["-f", "deploy/compose/docker-compose.build.yml"]);
        }
        for (key, _, value) in overrides {
            command.env(key, if variant == "fallback" { "" } else { value });
        }
        if let Some((key, value)) = negative {
            command.env(key, value);
        }
        let output = command.args(["config", "--format", "json"]).output()?;
        require(
            output.status.success(),
            format!("Compose render failed ({variant}/{mode}); configuration output withheld"),
        )?;
        strict_json(&output.stdout)
    };
    let mut cases = vec![];
    for variant in ["fallback", "overrides", "source_build"] {
        for mode in ["full", "compact", "scale"] {
            let config = render(mode, variant, None)?;
            check(&root, &config, mode, variant == "source_build")?;
            crate::database_identity::check_compose(&config, "ai_gateway_")?;
            if variant != "fallback" {
                for (_, service, image) in overrides {
                    require(
                        config["services"][service]["image"] == image,
                        format!("Operator image override ignored for {service}"),
                    )?;
                }
            }
            cases.push(json!({"case":format!("{variant}/{mode}"),"result":"pass"}));
        }
    }
    for (name, key, value) in [
        (
            "web_knowledge_collision",
            "FRONTEND_IMAGE",
            "example.invalid/upgrade/knowledge:2.0.0",
        ),
        (
            "stale_worker",
            "AGENT_CAPABILITY_WORKER_IMAGE",
            "ai-gateway-agent-capability-worker:local-obsolete",
        ),
    ] {
        let config = render("full", "overrides", Some((key, value)))?;
        require(
            check(&root, &config, "full", false).is_err(),
            format!("Compose negative case unexpectedly passed: {name}"),
        )?;
        cases.push(json!({"case":name,"result":"rejected_as_required"}));
    }
    Ok(
        json!({"result":"pass","tier":"L0","cases":cases,"artifact_acceptance":"not_checked","services_started":false}),
    )
}
