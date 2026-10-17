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
    Ok(
        json!({"result":"pass","mode":mode,"source_build":source_build,
        "identity":"configuration_matches_source_lock","artifact_acceptance":"not_checked; source-lock verify-local-images remains required"}),
    )
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
