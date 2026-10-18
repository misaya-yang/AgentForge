//! Node attestor for a measured gVisor runtime. This is software runtime evidence,
//! not a hardware/TEE claim. Kubernetes declarations alone are never signed:
//! admission also requires a live CRI sandbox and its pinned runsc executable,
//! matching sandbox ID, and an observed --network=none runtime argument.
use crate::sandbox_pool::{Kube, PoolConfig};
use ai_platform_capability_contract::lease_v3::{self as wire, PublicKeyring, SignedEnvelope};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttestorConfig {
    pub pool: PoolConfig,
    pub crictl: PathBuf,
    pub cri_endpoint: String,
    pub host_proc: PathBuf,
    pub runtime_sha256: String,
    pub signing_key_file: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Key {
    active_key_id: String,
    signing_seed_hex: String,
    verification: PublicKeyring,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Probe {
    schema: String,
    boot_id: String,
    workload_id: String,
}
fn bounded(path: &Path, max: usize) -> Result<Vec<u8>, &'static str> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| "attestor_measurement_unavailable")?;
    if !metadata.is_file() || metadata.len() > max as u64 {
        return Err("attestor_measurement_invalid");
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| "attestor_measurement_unavailable")?
        .take((max + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "attestor_measurement_unavailable")?;
    if bytes.len() > max {
        return Err("attestor_measurement_too_large");
    };
    Ok(bytes)
}
fn cri(config: &AttestorConfig, args: &[&str]) -> Result<Value, &'static str> {
    use std::process::{Command, Stdio};
    // A privileged read-only observer must never invoke a shell or a binary
    // selected by an untrusted workload. crictl and its socket are node policy.
    if !config.crictl.is_absolute() || !config.cri_endpoint.starts_with("unix:///") {
        return Err("attestor_cri_invalid");
    }
    let mut child = Command::new(&config.crictl)
        .arg("--runtime-endpoint")
        .arg(&config.cri_endpoint)
        .arg("--timeout=2s")
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "attestor_cri_unavailable")?;
    let stdout = child.stdout.take().ok_or("attestor_cri_unavailable")?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes);
        (result, bytes)
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() < Duration::from_secs(3) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err("attestor_cri_timeout");
            }
        }
    };
    let (read, bytes) = reader.join().map_err(|_| "attestor_cri_unavailable")?;
    if !status.success() || read.is_err() || bytes.len() > 2 * 1024 * 1024 {
        return Err("attestor_cri_rejected");
    }
    serde_json::from_slice(&bytes).map_err(|_| "attestor_cri_invalid")
}
fn pod_policy(pod: &Value, c: &PoolConfig) -> Result<(), &'static str> {
    let spec = &pod["spec"];
    if spec["nodeName"] != c.node_name
        || spec["runtimeClassName"] != c.runtime_class
        || spec["automountServiceAccountToken"] != false
        || spec["enableServiceLinks"] != false
        || spec["hostNetwork"] != false
        || spec["hostPID"] != false
        || spec["hostIPC"] != false
        || spec["restartPolicy"] != "Never"
        || !pod["metadata"]["deletionTimestamp"].is_null()
        || spec["containers"]
            .as_array()
            .is_none_or(|a| a.len() != 1 || a[0]["name"] != "sandbox" || a[0]["image"] != c.image)
        || spec["initContainers"].as_array().is_none_or(|a| {
            a.len() != 1 || a[0]["name"] != "measurement" || a[0]["image"] != c.image
        })
        || spec["ephemeralContainers"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
    {
        return Err("attestor_pod_policy_denied");
    }
    let name = pod["metadata"]["name"]
        .as_str()
        .ok_or("attestor_pod_invalid")?;
    if !name.starts_with("slot-")
        || name.len() != 37
        || !name[5..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("attestor_pod_invalid");
    }
    let expected_pod = c.pod(name);
    for kind in ["containers", "initContainers"] {
        let actual = &spec[kind][0];
        let expected = &expected_pod["spec"][kind][0];
        for field in [
            "name",
            "image",
            "command",
            "args",
            "volumeMounts",
            "securityContext",
            "resources",
        ] {
            if actual[field] != expected[field] {
                return Err("attestor_launch_spec_changed");
            }
        }
        for forbidden in ["lifecycle", "startupProbe", "ports", "envFrom"] {
            if actual.get(forbidden).is_some() {
                return Err("attestor_launch_spec_changed");
            }
        }
    }
    let expected = c.shared_root.join(name);
    let volumes = spec["volumes"]
        .as_array()
        .ok_or("attestor_volumes_invalid")?;
    if volumes.len() != 6 {
        return Err("attestor_volumes_invalid");
    }
    for volume in volumes {
        match volume["name"].as_str() {
            Some("slot")
                if volume["hostPath"]["path"].as_str() == expected.to_str()
                    && volume["hostPath"]["type"] == "Directory" => {}
            Some("public") if volume["configMap"]["name"] == c.attestation_config_map => {}
            Some("workspace" | "journal" | "tmp") if volume["emptyDir"]["medium"] == "Memory" => {}
            Some("cgroup") if volume["csi"] == c.cgroup_csi => {}
            _ => return Err("attestor_volumes_invalid"),
        }
    }
    for container in spec["containers"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(spec["initContainers"].as_array().into_iter().flatten())
    {
        let sc = &container["securityContext"];
        if sc["allowPrivilegeEscalation"] != false
            || sc["readOnlyRootFilesystem"] != true
            || sc["privileged"] == true
            || sc["capabilities"]["add"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
            || sc["capabilities"]["drop"] != json!(["ALL"])
            || container["envFrom"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
            || container["env"].as_array().is_some_and(|a| !a.is_empty())
            || container["command"] != json!(["/usr/bin/env"])
            || container["args"][0] != "-i"
        {
            return Err("attestor_container_policy_denied");
        }
    }
    if spec["securityContext"]["runAsNonRoot"] != true
        || spec["securityContext"]["runAsUser"] != 10001
        || spec["securityContext"]["seccompProfile"]["type"] != "Localhost"
        || spec["securityContext"]["seccompProfile"]["localhostProfile"] != c.seccomp_profile
    {
        return Err("attestor_container_policy_denied");
    }
    Ok(())
}
fn measured_runtime(
    config: &AttestorConfig,
    pod: &Value,
) -> Result<(String, String), &'static str> {
    pod_policy(pod, &config.pool)?;
    let uid = pod["metadata"]["uid"]
        .as_str()
        .ok_or("attestor_pod_uid_required")?;
    let pods = cri(config, &["pods", "-o", "json"])?;
    let sandbox = pods["items"]
        .as_array()
        .ok_or("attestor_cri_invalid")?
        .iter()
        .find(|s| s["metadata"]["uid"] == uid && s["state"] == "SANDBOX_READY")
        .ok_or("attestor_sandbox_not_running")?;
    let id = sandbox["id"]
        .as_str()
        .filter(|s| s.len() >= 32 && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("attestor_sandbox_id_invalid")?;
    let inspect = cri(config, &["inspectp", "-o", "json", id])?;
    if inspect["status"]["metadata"]["uid"] != uid
        || inspect["status"]["state"] != "SANDBOX_READY"
        || inspect["info"]["runtimeType"] != "io.containerd.runsc.v1"
    {
        return Err("attestor_runtime_handler_unmeasured");
    }
    // CRI may report runtimeHandler at either location depending on the runtime
    // version; at least one actual response field must match the admitted handler.
    if inspect["status"]["runtimeHandler"] != config.pool.runtime_handler
        && inspect["info"]["runtimeHandler"] != config.pool.runtime_handler
    {
        return Err("attestor_runtime_handler_unmeasured");
    }
    let containers = cri(config, &["ps", "-a", "-o", "json", "--pod", id])?;
    let records = containers["containers"]
        .as_array()
        .ok_or("attestor_container_measurement_missing")?;
    if records.len() != 2 {
        return Err("attestor_container_measurement_invalid");
    }
    let digest = config
        .pool
        .image
        .split('@')
        .nth(1)
        .ok_or("attestor_image_invalid")?;
    for (name, state) in [
        ("measurement", "CONTAINER_EXITED"),
        ("sandbox", "CONTAINER_RUNNING"),
    ] {
        let record = records
            .iter()
            .find(|r| r["metadata"]["name"] == name && r["state"] == state)
            .ok_or("attestor_container_not_running")?;
        if !record["imageRef"]
            .as_str()
            .is_some_and(|v| v == digest || v.ends_with(&format!("@{digest}")))
        {
            return Err("attestor_image_unmeasured");
        }
        if name == "measurement" {
            let container = record["id"]
                .as_str()
                .ok_or("attestor_container_id_invalid")?;
            if !container.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("attestor_container_id_invalid");
            }
            let status = cri(config, &["inspect", "-o", "json", container])?;
            if status["status"]["exitCode"] != 0 {
                return Err("attestor_probe_failed");
            }
        }
    }
    let mut measurement = None;
    for entry in std::fs::read_dir(&config.host_proc).map_err(|_| "attestor_host_proc_required")? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if !entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|b| b.is_ascii_digit())
        {
            continue;
        }
        let Ok(cmd) = std::fs::read(path.join("cmdline")) else {
            continue;
        };
        let args = cmd
            .split(|b| *b == 0)
            .filter_map(|v| std::str::from_utf8(v).ok())
            .collect::<Vec<_>>();
        if !args.iter().any(|v| *v == id) || !args.contains(&"--network=none") {
            continue;
        }
        // /proc/PID/exe reads the executable inode actually running, rather than
        // trusting a mutable path, pod annotation or process argv[0].
        let Ok(mut exe) = std::fs::File::open(path.join("exe")) else {
            continue;
        };
        let mut hash = Sha256::new();
        let mut buf = [0u8; 65536];
        let mut length = 0;
        loop {
            let n = exe
                .read(&mut buf)
                .map_err(|_| "attestor_runtime_measurement_failed")?;
            if n == 0 {
                break;
            };
            length += n;
            if length > 128 * 1024 * 1024 {
                return Err("attestor_runtime_measurement_too_large");
            };
            hash.update(&buf[..n]);
        }
        if hex::encode(hash.finalize()) == config.runtime_sha256 {
            let after = std::fs::read(path.join("cmdline"))
                .map_err(|_| "attestor_runtime_process_changed")?;
            if after != cmd {
                return Err("attestor_runtime_process_changed");
            };
            measurement = Some((
                id.to_owned(),
                entry.file_name().to_string_lossy().to_string(),
            ));
            break;
        }
    }
    measurement.ok_or("attestor_pinned_runsc_not_running")
}
fn attest(config: &AttestorConfig, pod: &Value) -> Result<(), &'static str> {
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    pod_policy(pod, &config.pool)?;
    let name = pod["metadata"]["name"]
        .as_str()
        .ok_or("attestor_pod_invalid")?;
    let dir = config.pool.shared_root.join(name);
    if std::fs::symlink_metadata(&dir)
        .map_err(|_| "attestor_slot_missing")?
        .file_type()
        .is_symlink()
    {
        return Err("attestor_slot_unsafe");
    }
    let evidence = dir.join("evidence.json");
    // Never renew a used sandbox's authority. Replacements get a new Pod UID and
    // a new slot. Evidence expiry requires destruction and fresh measurement.
    if evidence.exists() {
        return Ok(());
    }
    let (sandbox_id, pid) = measured_runtime(config, pod)?;
    let probe: Probe = serde_json::from_slice(&bounded(&dir.join("request.json"), 4096)?)
        .map_err(|_| "attestor_probe_invalid")?;
    if probe.schema != "sandbox-runtime-probe/v1"
        || probe.workload_id != name
        || uuid::Uuid::parse_str(&probe.boot_id).is_err()
    {
        return Err("attestor_probe_invalid");
    }
    // Resolve a projected Secret to one immutable generation inside its mount.
    let parent = std::fs::canonicalize(
        config
            .signing_key_file
            .parent()
            .ok_or("attestor_key_required")?,
    )
    .map_err(|_| "attestor_key_required")?;
    let key_path =
        std::fs::canonicalize(&config.signing_key_file).map_err(|_| "attestor_key_required")?;
    if !key_path.starts_with(&parent) {
        return Err("attestor_key_path_escape");
    }
    let meta = std::fs::symlink_metadata(&key_path).map_err(|_| "attestor_key_required")?;
    if !meta.is_file() || meta.mode() & 0o077 != 0 {
        return Err("attestor_private_key_permissions_invalid");
    }
    let key: Key =
        serde_json::from_slice(&bounded(&key_path, 16384)?).map_err(|_| "attestor_key_invalid")?;
    let now = crate::now_epoch_ms();
    let claims = json!({"schema":"sandbox-runtime-attestation/v2","runtime":"gvisor","boot_id":probe.boot_id,"workload_id":probe.workload_id,"pod_uid":pod["metadata"]["uid"],"image_digest":config.pool.image,"node_name":config.pool.node_name,"runtime_class":config.pool.runtime_class,"runtime_handler":config.pool.runtime_handler,"runtime_sha256":config.runtime_sha256,"cri_sandbox_id":sandbox_id,"runtime_pid":pid,"measurement":"cri+proc","network":"none","credentials":"none","one_execution":true});
    let envelope = SignedEnvelope::issue(
        &wire::unhex::<32>(&key.signing_seed_hex)?,
        &key.active_key_id,
        "sandbox-runtime-attestation",
        claims,
        now,
        now + 900_000,
        &key.verification,
    )?;
    // O_NOFOLLOW and exclusive creation prevent a sandbox-controlled symlink
    // from turning the privileged signer into an arbitrary file writer.
    let pending = dir.join(".evidence.pending");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&pending)
        .map_err(|_| "attestor_evidence_write_failed")?;
    file.write_all(&serde_json::to_vec(&envelope).map_err(|_| "attestor_evidence_invalid")?)
        .and_then(|_| file.sync_all())
        .map_err(|_| "attestor_evidence_write_failed")?;
    std::fs::rename(pending, evidence).map_err(|_| "attestor_evidence_write_failed")?;
    std::fs::File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(|_| "attestor_evidence_write_failed")?;
    Ok(())
}
pub async fn serve(config: AttestorConfig) -> Result<(), &'static str> {
    config.pool.validate()?;
    if !config.host_proc.is_absolute()
        || !config.signing_key_file.is_absolute()
        || config.runtime_sha256.len() != 64
        || !config.runtime_sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("attestor_configuration_invalid");
    }
    let kube = Kube::in_cluster()?;
    loop {
        let path = format!(
            "/api/v1/namespaces/{}/pods?labelSelector=agentforge.io%2Fsandbox-pool%3D{}&fieldSelector=spec.nodeName%3D{}",
            config.pool.namespace, config.pool.pool, config.pool.node_name
        );
        let class = kube
            .request(
                reqwest::Method::GET,
                &format!(
                    "/apis/node.k8s.io/v1/runtimeclasses/{}",
                    config.pool.runtime_class
                ),
                None,
            )
            .await?;
        if class["handler"] != config.pool.runtime_handler {
            return Err("attestor_runtimeclass_not_admitted");
        }
        let pods = kube.request(reqwest::Method::GET, &path, None).await?;
        for pod in pods["items"].as_array().ok_or("attestor_pods_invalid")? {
            let owned = config.clone();
            let pod = pod.clone();
            match tokio::task::spawn_blocking(move || attest(&owned, &pod)).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::debug!(error, "sandbox runtime not yet attested"),
                Err(_) => return Err("attestor_measurement_task_failed"),
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admitted_pod_template_rejects_runtime_image_credentials_mount_and_launcher_mutation() {
        let config:PoolConfig=serde_json::from_value(json!({"namespace":"sandbox","node_name":"node","pool":"strong","runtime_class":"gvisor","runtime_handler":"runsc","image":format!("sandbox@sha256:{}","a".repeat(64)),"shared_root":"/var/lib/slots","socket":"/var/lib/control.sock","journal_root":"/var/lib/journal","warm":1,"maximum":2,"retention_ms":600000,"public_key_id":"public","public_key_hex":"a".repeat(64),"attestation_config_map":"public","seccomp_profile":"sandbox.json","cgroup_csi":{"driver":"fixture.cgroup"}})).unwrap();
        let pod = config.pod("slot-00000000000000000000000000000000");
        assert!(pod_policy(&pod, &config).is_ok());
        for (pointer, value) in [
            ("/spec/runtimeClassName", json!("runc")),
            ("/spec/automountServiceAccountToken", json!(true)),
            ("/spec/hostNetwork", json!(true)),
            ("/spec/containers/0/image", json!("untrusted:latest")),
            ("/spec/containers/0/args", json!(["-i", "/bin/sh"])),
            ("/spec/volumes/0/hostPath/path", json!("/")),
            (
                "/spec/containers/0/securityContext/allowPrivilegeEscalation",
                json!(true),
            ),
        ] {
            let mut bad = pod.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(pod_policy(&bad, &config).is_err(), "{pointer}");
        }
        let mut bad = pod.clone();
        bad["spec"]["containers"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"sidecar","image":"untrusted"}));
        assert!(pod_policy(&bad, &config).is_err());
        let mut bad = pod;
        bad["spec"]["containers"][0]["env"] =
            json!([{"name":"AI_PLATFORM_INTERNAL_TOKEN","value":"fixture"}]);
        assert!(pod_policy(&bad, &config).is_err());
    }
    #[test]
    fn declarations_cannot_replace_runtime_measurement() {
        // Even a correctly labelled RuntimeClass and pod never suffice without
        // an actual CRI sandbox, pinned executable and network=none evidence.
        let dir = std::env::temp_dir().join(format!("attestor-fixture-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&dir).unwrap();
        let pool:PoolConfig=serde_json::from_value(json!({"namespace":"sandbox","node_name":"node","pool":"strong","runtime_class":"gvisor","runtime_handler":"runsc","image":format!("sandbox@sha256:{}","a".repeat(64)),"shared_root":dir,"socket":"/tmp/pool.sock","journal_root":"/tmp/pool-journal","warm":1,"maximum":2,"retention_ms":600000,"public_key_id":"public","public_key_hex":"a".repeat(64),"attestation_config_map":"public","seccomp_profile":"sandbox.json","cgroup_csi":{"driver":"local.cgroup"}})).unwrap();
        let config = AttestorConfig {
            pool,
            crictl: dir.join("absent-crictl"),
            cri_endpoint: "unix:///run/containerd/containerd.sock".into(),
            host_proc: dir.clone(),
            runtime_sha256: "a".repeat(64),
            signing_key_file: dir.join("absent-key"),
        };
        assert!(
            measured_runtime(
                &config,
                &json!({"metadata":{"name":"slot-forged"},"spec":{"runtimeClassName":"gvisor"}})
            )
            .is_err()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
