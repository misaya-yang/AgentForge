//! Separate artifact identity and complete Runtime/Worker/Sandbox release admission.
use super::*;
pub(super) const ARTIFACTS: &[(&str, &str)] = &[
    ("app_server", "codex-app-server"),
    ("agent_runtime", "ai-platform-agent-runtime"),
    ("capability_worker", "ai-platform-capability-worker"),
    ("python_sandbox", "ai-platform-python-sandbox"),
];
const RELEASE: &[(&str, &str, &str)] = &[
    (
        "agent_runtime",
        "--runtime-image",
        "ai-gateway-agent-runtime",
    ),
    (
        "capability_worker",
        "--worker-image",
        "ai-gateway-agent-capability-worker",
    ),
    (
        "python_sandbox",
        "--sandbox-image",
        "ai-gateway-agent-python-sandbox",
    ),
];
pub(super) fn protocol(id: &str) -> Option<&'static str> {
    match id {
        "app_server" => Some("codex-app-server-jsonrpc/v2"),
        "agent_runtime" => Some("ai-platform-agent-runtime-http-sse/v1"),
        "capability_worker" => Some("ai-platform-capability-contract/v2"),
        "python_sandbox" => Some("sandbox-job/v1"),
        _ => None,
    }
}
fn current_inventory(lock: &Value) -> Result<()> {
    let artifacts = lock["oci"]["artifacts"]
        .as_object()
        .ok_or("OCI artifacts missing")?;
    require(
        artifacts.len() == ARTIFACTS.len()
            && ARTIFACTS.iter().all(|(id, _)| artifacts.contains_key(*id)),
        "current source release requires all four OCI artifacts including python_sandbox",
    )
}
pub(super) fn validate_artifacts(
    lock: &Value,
    require_runnable: bool,
    required: Option<&str>,
) -> Result<Value> {
    let state = text(lock, "release_state")?;
    require(
        matches!(
            state,
            "local_source_locked" | "local_image_locked" | "published"
        ),
        "invalid release state",
    )?;
    let artifacts = lock["oci"]["artifacts"]
        .as_object()
        .ok_or("OCI artifacts missing")?;
    let historical = lock["source"]["upstream_sha"] == "092d3acd6bec3e3a14bdc7e7a2810ab628ab759d"
        && artifacts.len() == 3
        && ARTIFACTS[..3]
            .iter()
            .all(|(id, _)| artifacts.contains_key(*id));
    if historical {
        require(
            !require_runnable && required.is_none(),
            "historical three-artifact source lock is audit-only",
        )?;
    } else {
        current_inventory(lock)?;
    }
    let mut runnable = BTreeSet::new();
    for &(id, binary) in ARTIFACTS
        .iter()
        .take(if historical { 3 } else { ARTIFACTS.len() })
    {
        let artifact = &artifacts[id];
        require(artifact["binary"] == binary, "OCI binary mismatch")?;
        if historical {
            text(artifact, "protocol")?;
        } else {
            require(
                artifact["protocol"].as_str() == protocol(id),
                "OCI protocol mismatch",
            )?;
            require(
                artifact["candidate_start_allowed"].is_boolean(),
                "OCI admission flag must be boolean",
            )?;
        }
        require(
            artifact["platforms"].as_array().is_some_and(|platforms| {
                (historical || !platforms.is_empty())
                    && platforms
                        .iter()
                        .all(|p| p.as_str().is_some_and(|p| !p.is_empty()))
            }),
            "OCI platforms invalid",
        )?;
        if artifact["candidate_start_allowed"] != true {
            require(
                artifact["image_digest"].is_null() && artifact["image_ref"].is_null(),
                "unlocked artifact carries image identity",
            )?;
            continue;
        }
        let digest = text(artifact, "image_digest")?;
        require(
            digest
                .strip_prefix("sha256:")
                .is_some_and(|sha| hex(sha, 64)),
            "image digest must be immutable",
        )?;
        let image = text(artifact, "image_ref")?;
        require(
            match state {
                "local_image_locked" => image == format!("local-image://{digest}"),
                "published" => image.ends_with(&format!("@{digest}")),
                _ => false,
            },
            "runnable artifact release identity invalid",
        )?;
        runnable.insert(id);
    }
    require(
        if state == "local_source_locked" {
            runnable.is_empty()
        } else {
            !runnable.is_empty()
        },
        "release state disagrees with runnable artifacts",
    )?;
    let complete = !historical && RELEASE.iter().all(|(id, _, _)| runnable.contains(id));
    if !historical && (require_runnable || state == "published") {
        require(
            complete,
            "current release requires runnable Runtime, Worker and Python Sandbox",
        )?;
    }
    if let Some(id) = required.or(if require_runnable {
        Some("app_server")
    } else {
        None
    }) {
        require(
            runnable.contains(id),
            "required artifact not locked runnable",
        )?;
    }
    Ok(
        json!({"result":"pass","source_lock":"validated","release_state":state,"runnable_artifacts":runnable,
        "historical_audit_only":historical,"release_admission":complete}),
    )
}
fn checked_image(lock: &Value, id: &str, info: &Value) -> Result<(String, String)> {
    let binary = ARTIFACTS
        .iter()
        .find(|(key, _)| *key == id)
        .map(|(_, binary)| *binary)
        .ok_or("unknown OCI artifact")?;
    let build = &lock["build"];
    let source = &lock["source"];
    let labels = &info["Config"]["Labels"];
    let worker_family = matches!(id, "capability_worker" | "python_sandbox");
    if id == "python_sandbox" {
        require(
            build["overlay_sha256"]
                .as_str()
                .is_some_and(|sha| hex(sha, 64))
                && build["capability_worker_schema_sha256"]
                    .as_str()
                    .is_some_and(|sha| hex(sha, 64))
                && source["upstream_sha"]
                    .as_str()
                    .is_some_and(|sha| hex(sha, 40)),
            "sandbox source/overlay/schema identities must be immutable",
        )?;
    }
    let revision = if let Some(sha) = build["overlay_sha256"].as_str() {
        format!(
            "{}+{}",
            text(source, "upstream_sha")?,
            &sha[..sha.len().min(12)]
        )
    } else {
        text(source, "fork_sha")?.into()
    };
    let prefix = format!(
        "com.misaya.ai-platform.{}",
        if worker_family {
            "capability-worker"
        } else {
            "agent-runtime"
        }
    );
    require(
        labels["org.opencontainers.image.revision"] == revision
            && labels[format!("{prefix}.schema-sha256")]
                == build[if worker_family {
                    "capability_worker_schema_sha256"
                } else {
                    "app_server_schema_sha256"
                }]
            && labels[format!("{prefix}.artifact")] == id
            && labels[format!("{prefix}.binary")] == binary,
        "Docker labels differ from locked source identity",
    )?;
    if worker_family && !build["overlay_sha256"].is_null() {
        require(
            labels[format!("{prefix}.overlay-sha256")] == build["overlay_sha256"],
            "Docker overlay label mismatch",
        )?;
    }
    let digest = text(info, "Id")?;
    require(
        digest
            .strip_prefix("sha256:")
            .is_some_and(|sha| hex(sha, 64)),
        "Docker image Id is not immutable",
    )?;
    Ok((
        digest.into(),
        format!("{}/{}", text(info, "Os")?, text(info, "Architecture")?),
    ))
}
fn verify_inspections(lock: &Value, inspections: &[(&str, Value)]) -> Result<Value> {
    current_inventory(lock)?;
    for &(id, _, _) in RELEASE {
        validate_artifacts(lock, false, Some(id))?;
    }
    let ids: BTreeSet<_> = inspections.iter().map(|(id, _)| *id).collect();
    require(
        inspections.len() == RELEASE.len()
            && ids.len() == RELEASE.len()
            && RELEASE.iter().all(|(id, _, _)| ids.contains(id)),
        "release image inspection inventory differs",
    )?;
    let mut platforms = BTreeSet::new();
    let mut digests = BTreeSet::new();
    for (id, info) in inspections {
        let (digest, platform) = checked_image(lock, id, info)?;
        let artifact = &lock["oci"]["artifacts"][*id];
        if lock["release_state"] == "published" {
            require(
                info["RepoDigests"]
                    .as_array()
                    .is_some_and(|refs| refs.contains(&artifact["image_ref"])),
                "registry digest differs from lock",
            )?;
        } else {
            require(
                artifact["image_digest"] == digest,
                "local image digest differs from lock",
            )?;
        }
        require(
            artifact["platforms"]
                .as_array()
                .is_some_and(|values| values.contains(&json!(platform))),
            "Docker platform differs from lock",
        )?;
        require(
            digests.insert(digest),
            "one image cannot satisfy multiple release artifacts",
        )?;
        platforms.insert(platform);
    }
    require(
        platforms.len() == 1,
        "Runtime, capability worker and Python Sandbox platform mismatch",
    )?;
    Ok(json!({"result":"pass","release_unit":"validated","artifacts":ids,"platforms":platforms}))
}
pub fn verify_images(args: &Args) -> Result<Value> {
    let root = args.root();
    let path = Path::new(args.get("--lock")?);
    for &(id, _, _) in RELEASE {
        validate_lock(&root, path, false, Some(id))?;
    }
    let lock = object(path)?;
    let upstream = text(&lock["source"], "upstream_sha")?;
    let overlay = lock["build"]["overlay_sha256"].as_str().unwrap_or("");
    let suffix = format!(
        "local-{}-{}",
        &upstream[..upstream.len().min(12)],
        &overlay[..overlay.len().min(12)]
    );
    let mut inspections = Vec::new();
    for &(id, option, prefix) in RELEASE {
        let image = args
            .values
            .get(option)
            .cloned()
            .unwrap_or_else(|| format!("{prefix}:{suffix}"));
        let output = Command::new("docker")
            .args(["image", "inspect", &image])
            .current_dir(&root)
            .output()?;
        require(output.status.success(), "Docker image inspection failed")?;
        let result = strict_json(&output.stdout)?;
        let info = result
            .as_array()
            .filter(|values| values.len() == 1)
            .and_then(|values| values.first())
            .ok_or("unexpected Docker inspect shape")?;
        inspections.push((id, info.clone()));
    }
    verify_inspections(&lock, &inspections)
}
pub fn record_image(args: &Args) -> Result<Value> {
    let root = args.root();
    let path = Path::new(args.get("--lock")?);
    let artifact = args.get("--artifact")?;
    require(
        ARTIFACTS.iter().any(|(id, _)| *id == artifact),
        "unknown OCI artifact",
    )?;
    validate_lock(&root, path, false, None)?;
    let mut lock = object(path)?;
    current_inventory(&lock)?;
    let output = Command::new("docker")
        .args(["image", "inspect", args.get("--image")?])
        .current_dir(&root)
        .output()?;
    require(output.status.success(), "Docker image inspection failed")?;
    let info = strict_json(&output.stdout)?;
    let info = info
        .as_array()
        .filter(|values| values.len() == 1)
        .and_then(|values| values.first())
        .ok_or("unexpected Docker inspection shape")?;
    let (digest, platform) = checked_image(&lock, artifact, info)?;
    let value = &mut lock["oci"]["artifacts"][artifact];
    value["candidate_start_allowed"] = json!(true);
    value["image_ref"] = json!(format!("local-image://{digest}"));
    value["image_digest"] = json!(digest);
    value["platforms"] = json!([platform]);
    lock["release_state"] = json!("local_image_locked");
    validate_artifacts(&lock, false, Some(artifact))?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    use std::io::Write;
    lock.sort_all_objects();
    file.write_all(format!("{}\n", serde_json::to_string_pretty(&lock)?).as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    std::fs::File::open(path.parent().unwrap_or(Path::new(".")))?.sync_all()?;
    validate_lock(&root, path, false, Some(artifact))?;
    Ok(json!({"result":"pass","artifact":artifact,"release_state":"local_image_locked"}))
}
#[cfg(test)]
#[path = "source_images_tests.rs"]
mod tests;
