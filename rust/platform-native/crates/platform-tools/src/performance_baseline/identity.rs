use super::{
    Result,
    config::{self, Profile},
    ensure, evidence, sha,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::AsyncReadExt;
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRef {
    pub path: PathBuf,
    pub sha256: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gate {
    pub document: FileRef,
    pub pointer: String,
    pub expected: Value,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    pub id: String,
    pub telemetry_service: String,
    pub instance_id: String,
    pub pid: u32,
    pub start_ticks: u64,
    pub uid: u32,
    pub container_id: String,
    pub image_id: String,
    pub binary_sha256: String,
    pub environment_sha256: String,
    pub materials: Vec<FileRef>,
    pub network_addresses: Vec<String>,
    pub runtime_settings: Value,
    pub listener_port: Option<u16>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accepted {
    pub schema: String,
    pub accepted: bool,
    pub scope: String,
    pub expires_unix: i64,
    pub profile_sha256: String,
    pub controller_binary_sha256: String,
    pub source_manifest: FileRef,
    pub source_roots: Vec<String>,
    pub boot_id: String,
    pub compose_project: String,
    pub deployment_working_dir: PathBuf,
    pub services: Vec<Service>,
    pub gates: BTreeMap<String, Gate>,
    pub docker_binary: FileRef,
    pub docker_socket: PathBuf,
    pub real_provider_authorized: bool,
}
pub struct Prepared {
    pub profile: Profile,
    pub accepted: Accepted,
    pub root: PathBuf,
    pub profile_sha256: String,
    pub receipt_sha256: String,
    pub pinned_files: Vec<FileRef>,
}
pub fn verify_signature(receipt: &[u8], signature: &[u8], key: &[u8]) -> Result<()> {
    let k: [u8; 32] = key.try_into().map_err(|_| "acceptance_key_shape")?;
    let key = VerifyingKey::from_bytes(&k).map_err(|_| "acceptance_key_invalid")?;
    let signature = Signature::from_slice(signature).map_err(|_| "acceptance_signature_shape")?;
    let mut message = b"agentforge-performance-accepted-build/v1\0".to_vec();
    message.extend_from_slice(receipt);
    key.verify_strict(&message, &signature)
        .map_err(|_| "acceptance_signature_invalid".into())
}
fn signature_bytes(path: &Path) -> Result<Vec<u8>> {
    let raw = evidence::read(path, true, 4096)?;
    if raw.len() == 64 {
        Ok(raw)
    } else {
        URL_SAFE_NO_PAD
            .decode(
                std::str::from_utf8(&raw)
                    .map_err(|_| "signature_encoding")?
                    .trim(),
            )
            .map_err(|_| "signature_encoding".into())
    }
}
pub fn prepare(args: &BTreeMap<String, String>) -> Result<Prepared> {
    let profile_path = Path::new(&args["--profile"]);
    let profile_bytes = evidence::read(profile_path, true, 2 * 1024 * 1024)?;
    let profile: Profile =
        serde_json::from_slice(&profile_bytes).map_err(|_| "profile_json_invalid")?;
    config::validate_profile(&profile)?;
    let receipt_path = Path::new(&args["--acceptance"]);
    let bytes = evidence::read(receipt_path, true, 8 * 1024 * 1024)?;
    let key_path = Path::new(&args["--trust-key"]);
    let key = evidence::read(key_path, false, 4096)?;
    ensure(
        config::digest(&args["--trust-key-sha256"]) && sha(&key) == args["--trust-key-sha256"],
        "externally_pinned_acceptance_key_required",
    )?;
    verify_signature(
        &bytes,
        &signature_bytes(Path::new(&args["--signature"]))?,
        &key,
    )?;
    let accepted: Accepted =
        serde_json::from_slice(&bytes).map_err(|_| "acceptance_schema_invalid")?;
    validate_acceptance(&accepted, &profile, sha(&profile_bytes).as_str())?;
    let root = Path::new(&args["--repo-root"])
        .canonicalize()
        .map_err(|_| "repository_root_unavailable")?;
    verify_sources(&root, &accepted)?;
    ensure(
        evidence::file_hash(&std::env::current_exe().map_err(|_| "controller_executable")?)?
            == accepted.controller_binary_sha256,
        "controller_binary_not_accepted",
    )?;
    for gate in accepted.gates.values() {
        let v = evidence::checked_json(&gate.document.path, &gate.document.sha256, true)?;
        ensure(
            v.pointer(&gate.pointer) == Some(&gate.expected),
            "accepted_gate_evidence_changed",
        )?;
    }
    let mut pinned_files = vec![
        FileRef {
            path: profile_path.into(),
            sha256: sha(&profile_bytes),
        },
        FileRef {
            path: receipt_path.into(),
            sha256: sha(&bytes),
        },
        FileRef {
            path: key_path.into(),
            sha256: sha(&key),
        },
        accepted.source_manifest.clone(),
    ];
    pinned_files.extend(accepted.gates.values().map(|g| g.document.clone()));
    if let Some(config) = &profile.accounting {
        pinned_files.push(FileRef {
            path: config.config_file.clone(),
            sha256: config.config_sha256.clone(),
        });
    }
    Ok(Prepared {
        profile,
        accepted,
        root,
        profile_sha256: sha(&profile_bytes),
        receipt_sha256: sha(&bytes),
        pinned_files,
    })
}
pub fn validate_acceptance(a: &Accepted, p: &Profile, profile_hash: &str) -> Result<()> {
    ensure(
        a.schema == "agentforge-accepted-performance-build/v1"
            && a.accepted
            && a.scope == "isolated-owned-performance-fixture",
        "accepted_build_required",
    )?;
    ensure(
        config::safe_id(&a.compose_project) && a.deployment_working_dir.is_absolute(),
        "accepted_deployment_owner_required",
    )?;
    let now = chrono::Utc::now().timestamp();
    ensure(
        a.expires_unix > now
            && a.expires_unix <= now + 86400
            && a.profile_sha256 == profile_hash
            && config::digest(&a.controller_binary_sha256),
        "accepted_profile_or_time_mismatch",
    )?;
    ensure(
        a.services.iter().map(|s| &s.id).collect::<BTreeSet<_>>()
            == p.services.iter().collect::<BTreeSet<_>>()
            && a.services.len() == p.services.len(),
        "accepted_service_inventory_mismatch",
    )?;
    for gate in [
        "build_acceptance",
        "privilege_tenant_acl",
        "fixture_ownership",
        "accounting",
    ] {
        let g = a.gates.get(gate).ok_or("accepted_gate_evidence_missing")?;
        ensure(
            g.expected == json!(true)
                || g.expected == json!("pass")
                || g.expected == json!("accepted"),
            "accepted_gate_must_prove_success",
        )?;
    }
    ensure(
        a.services.iter().any(|s| s.id == "trace-backend"),
        "owned_trace_backend_required",
    )?;
    ensure(
        a.services
            .iter()
            .map(|s| s.pid)
            .collect::<BTreeSet<_>>()
            .len()
            == a.services.len()
            && a.services
                .iter()
                .map(|s| &s.container_id)
                .collect::<BTreeSet<_>>()
                .len()
                == a.services.len()
            && a.services
                .iter()
                .map(|s| &s.telemetry_service)
                .collect::<BTreeSet<_>>()
                .len()
                == a.services.len()
            && a.services
                .iter()
                .map(|s| &s.instance_id)
                .collect::<BTreeSet<_>>()
                .len()
                == a.services.len(),
        "duplicated_service_process_container_or_telemetry_identity",
    )?;
    for s in &a.services {
        ensure(
            config::safe_id(&s.id)
                && config::safe_id(&s.telemetry_service)
                && config::safe_id(&s.instance_id)
                && s.pid > 0
                && s.start_ticks > 0
                && s.uid > 0
                && config::digest(&s.container_id)
                && s.image_id
                    .strip_prefix("sha256:")
                    .is_some_and(config::digest)
                && config::digest(&s.binary_sha256)
                && config::digest(&s.environment_sha256),
            "accepted_process_identity_incomplete",
        )?;
        ensure(
            s.runtime_settings.is_object()
                && s.runtime_settings["privileged"] == false
                && s.runtime_settings["memory_limit_bytes"].as_u64().is_some(),
            "accepted_runtime_limits_required",
        )?;
    }
    Ok(())
}
fn inventory(root: &Path, relative: &str, files: &mut BTreeSet<String>) -> Result<()> {
    let path = evidence::relative(root, relative)?;
    let meta = std::fs::symlink_metadata(&path).map_err(|_| "source_metadata")?;
    if meta.is_dir() {
        for item in std::fs::read_dir(&path).map_err(|_| "source_inventory")? {
            let item = item.map_err(|_| "source_inventory")?;
            let name = item.file_name().into_string().map_err(|_| "source_name")?;
            if ["target", ".git", "node_modules", ".venv"].contains(&name.as_str()) {
                continue;
            }
            ensure(
                (!name.starts_with(".env") || name.ends_with(".example"))
                    && !name.ends_with(".p12")
                    && !name.ends_with(".pfx"),
                "private_source_material_refused",
            )?;
            inventory(root, &format!("{relative}/{name}"), files)?;
        }
    } else {
        ensure(
            meta.is_file() && files.insert(relative.into()),
            "overlapping_source_roots",
        )?;
    }
    Ok(())
}
pub fn verify_sources(root: &Path, a: &Accepted) -> Result<()> {
    let manifest =
        evidence::checked_json(&a.source_manifest.path, &a.source_manifest.sha256, true)?;
    ensure(
        manifest["schema"] == "agentforge-performance-source-manifest/v1",
        "source_manifest_schema",
    )?;
    let expected: BTreeMap<String, String> =
        serde_json::from_value(manifest["files"].clone()).map_err(|_| "source_manifest_files")?;
    let mut actual = BTreeSet::new();
    for prefix in &a.source_roots {
        ensure(
            prefix.starts_with("rust/")
                || prefix.starts_with("database/")
                || prefix.starts_with("deploy/"),
            "explicit_public_build_source_roots_required",
        )?;
        inventory(root, prefix, &mut actual)?;
    }
    ensure(
        actual == expected.keys().cloned().collect(),
        "source_inventory_changed",
    )?;
    for anchor in [
        "rust/platform-native/Cargo.toml",
        "rust/platform-native/Cargo.lock",
        "rust/platform-native/crates/platform-gateway/src/lib.rs",
        "rust/platform-native/crates/platform-knowledge-domain/src/lib.rs",
        "rust/platform-native/crates/platform-service-primitives/src/lib.rs",
    ] {
        ensure(
            actual.contains(anchor),
            "accepted_source_inventory_incomplete",
        )?;
    }
    for (path, digest) in expected {
        ensure(
            config::digest(&digest)
                && evidence::file_hash(&evidence::relative(root, &path)?)? == digest,
            "source_bytes_changed",
        )?;
    }
    Ok(())
}
async fn inspect(a: &Accepted, s: &Service) -> Result<Value> {
    ensure(
        evidence::file_hash(&a.docker_binary.path)? == a.docker_binary.sha256,
        "inspector_binary_changed",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        let m = std::fs::symlink_metadata(&a.docker_socket)
            .map_err(|_| "owned_docker_socket_unavailable")?;
        ensure(
            m.file_type().is_socket() && m.permissions().mode() & 0o002 == 0,
            "local_docker_socket_required",
        )?;
    }
    let format = r#"{"pid":{{.State.Pid}},"image":{{json .Image}},"id":{{json .Id}},"working_dir":{{json (index .Config.Labels "com.docker.compose.project.working_dir")}},"project":{{json (index .Config.Labels "com.docker.compose.project")}},"ports":{{json .NetworkSettings.Ports}},"networks":{{json .NetworkSettings.Networks}},"network_mode":{{json .HostConfig.NetworkMode}},"runtime_settings":{"memory_limit_bytes":{{.HostConfig.Memory}},"nano_cpus":{{.HostConfig.NanoCpus}},"cpu_quota":{{.HostConfig.CpuQuota}},"cpu_period":{{.HostConfig.CpuPeriod}},"privileged":{{.HostConfig.Privileged}},"readonly_rootfs":{{.HostConfig.ReadonlyRootfs}},"security_opt":{{json .HostConfig.SecurityOpt}}}}"#;
    let mut child = tokio::process::Command::new(&a.docker_binary.path)
        .arg("--host")
        .arg(format!("unix://{}", a.docker_socket.display()))
        .args(["container", "inspect", "--format", format, &s.container_id])
        .env_remove("DOCKER_HOST")
        .env_remove("DOCKER_CONTEXT")
        .env_remove("DOCKER_CONFIG")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "read_only_container_inspection_unavailable")?;
    let mut bytes = Vec::new();
    let mut stdout = child.stdout.take().ok_or("inspect_output_missing")?;
    tokio::time::timeout(Duration::from_secs(5), async {
        (&mut stdout)
            .take(8193)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "inspect_read")?;
        ensure(bytes.len() <= 8192, "inspect_output_budget")?;
        let status = child.wait().await.map_err(|_| "inspect_wait")?;
        ensure(status.success(), "read_only_container_inspection_failed")
    })
    .await
    .map_err(|_| "inspect_timeout")??;
    serde_json::from_slice(&bytes).map_err(|_| "inspect_json_invalid".into())
}
pub fn verify_process(s: &Service) -> Result<()> {
    let proc = PathBuf::from(format!("/proc/{}", s.pid));
    crate::performance::ProcessIdentity {
        pid: s.pid,
        start_ticks: s.start_ticks,
    }
    .sample()
    .map_err(|_| "process_identity_or_sampling_failed")?;
    let status = std::fs::read_to_string(proc.join("status"))
        .map_err(|_| "process_privilege_unavailable")?;
    let uid = status
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|n| n.parse::<u32>().ok());
    let caps = status
        .lines()
        .find_map(|l| l.strip_prefix("CapEff:"))
        .map(str::trim);
    ensure(
        uid == Some(s.uid) && caps == Some("0000000000000000"),
        "process_privilege_changed",
    )?;
    ensure(
        evidence::file_hash(&proc.join("exe"))? == s.binary_sha256,
        "live_process_binary_changed",
    )?;
    ensure(
        sha(&std::fs::read(proc.join("environ")).map_err(|_| "process_profile_unavailable")?)
            == s.environment_sha256,
        "live_environment_profile_changed",
    )?;
    for file in &s.materials {
        ensure(
            evidence::file_hash(&file.path)? == file.sha256,
            "runtime_material_changed",
        )?;
    }
    Ok(())
}
fn verify_listener(s: &Service, origin: &str, inspection: &Value) -> Result<()> {
    let origin = config::origin(origin)?;
    let port = s
        .listener_port
        .ok_or("accepted_internal_listener_required")?;
    let mapped = inspection["ports"][format!("{port}/tcp")]
        .as_array()
        .is_some_and(|mappings| {
            mappings.iter().any(|v| {
                v["HostIp"] == "127.0.0.1"
                    && v["HostPort"].as_str() == Some(&origin.port().unwrap().to_string())
            })
        });
    let same_namespace = std::fs::read_link(format!("/proc/{}/ns/net", s.pid))
        .ok()
        .zip(std::fs::read_link("/proc/self/ns/net").ok())
        .is_some_and(|(a, b)| a == b);
    ensure(
        mapped || (same_namespace && origin.port() == Some(port)),
        "owned_port_mapping_or_shared_namespace_changed",
    )?;
    let proc = PathBuf::from(format!("/proc/{}", s.pid));
    let mut inodes = BTreeSet::new();
    for entry in std::fs::read_dir(proc.join("fd")).map_err(|_| "listener_fd_unavailable")? {
        if let Ok(e) = entry {
            if let Ok(link) = std::fs::read_link(e.path()) {
                let t = link.to_string_lossy();
                if let Some(i) = t.strip_prefix("socket:[").and_then(|s| s.strip_suffix(']')) {
                    inodes.insert(i.to_owned());
                }
            }
        }
    }
    let mut owned = false;
    for file in ["net/tcp", "net/tcp6"] {
        let text =
            std::fs::read_to_string(proc.join(file)).map_err(|_| "listener_table_unavailable")?;
        for line in text.lines().skip(1) {
            let f = line.split_whitespace().collect::<Vec<_>>();
            if f.len() > 9
                && f[3] == "0A"
                && f[1]
                    .rsplit_once(':')
                    .and_then(|(_, p)| u16::from_str_radix(p, 16).ok())
                    == Some(port)
                && inodes.contains(f[9])
            {
                owned = true;
            }
        }
    }
    ensure(owned, "actual_owned_service_listener_missing")
}
pub async fn verify_live(p: &Prepared) -> Result<()> {
    ensure(
        cfg!(target_os = "linux"),
        "linux_same_pid_namespace_required",
    )?;
    ensure(
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map_err(|_| "boot_identity_unavailable")?
            .trim()
            == p.accepted.boot_id,
        "host_boot_identity_changed",
    )?;
    ensure(
        chrono::Utc::now().timestamp() < p.accepted.expires_unix,
        "accepted_build_expired",
    )?;
    for file in &p.pinned_files {
        ensure(
            evidence::file_hash(&file.path)? == file.sha256,
            "pinned_evidence_changed",
        )?;
    }
    verify_sources(&p.root, &p.accepted)?;
    for s in &p.accepted.services {
        verify_process(s)?;
        let current = inspect(&p.accepted, s).await?;
        ensure(
            current["pid"].as_u64().is_some_and(|pid| pid > 0)
                && current["image"] == s.image_id
                && current["id"] == s.container_id
                && current["working_dir"].as_str() == p.accepted.deployment_working_dir.to_str()
                && current["project"] == p.accepted.compose_project,
            "live_container_image_or_owner_changed",
        )?;
        let cgroup = std::fs::read_to_string(format!("/proc/{}/cgroup", s.pid))
            .map_err(|_| "service_cgroup_identity_unavailable")?;
        ensure(
            current["pid"].as_u64() == Some(s.pid as u64) || cgroup.contains(&s.container_id),
            "service_process_not_in_accepted_container",
        )?;
        ensure(
            current["runtime_settings"] == s.runtime_settings,
            "live_resource_or_security_profile_changed",
        )?;
        let mut addresses = current["networks"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.values())
            .filter_map(|v| v["IPAddress"].as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        addresses.sort();
        let mut expected = s.network_addresses.clone();
        expected.sort();
        ensure(addresses == expected, "container_network_identity_changed")?;
        if s.id == "gateway" {
            verify_listener(s, &p.profile.gateway_origin, &current)?;
        }
        if s.id == "trace-backend" {
            verify_listener(s, &p.profile.trace_origin, &current)?;
        }
    }
    Ok(())
}
