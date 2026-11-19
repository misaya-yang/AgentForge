//! Node-local strong sandbox pool. Kubernetes controls lifecycle; sealed jobs use
//! Unix sockets exclusively. Durable reservations never reassign a job after an
//! acknowledgement loss or controller restart.
use crate::sandbox_transport::MAX_SANDBOX_WIRE_BYTES;
use ai_platform_capability_contract::sandbox_job::{
    SandboxJobVerifier, SandboxMessage, SealedSandboxMessage,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Mutex;
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolConfig {
    pub namespace: String,
    pub node_name: String,
    pub pool: String,
    pub runtime_class: String,
    pub runtime_handler: String,
    pub image: String,
    pub shared_root: PathBuf,
    pub socket: PathBuf,
    pub journal_root: PathBuf,
    pub warm: usize,
    pub maximum: usize,
    pub retention_ms: u64,
    pub public_key_id: String,
    pub public_key_hex: String,
    pub attestation_config_map: String,
    pub seccomp_profile: String,
    pub cgroup_csi: Value,
}
#[derive(Clone)]
pub struct Kube {
    client: reqwest::Client,
    base: String,
    token_file: PathBuf,
}
impl Kube {
    pub fn in_cluster() -> Result<Self, &'static str> {
        let token_file = PathBuf::from("/var/run/secrets/kubernetes.io/serviceaccount/token");
        let ca = std::fs::read("/var/run/secrets/kubernetes.io/serviceaccount/ca.crt")
            .map_err(|_| "pool_kubernetes_ca_required")?;
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .tls_certs_only([reqwest::Certificate::from_pem(&ca).map_err(|_| "pool_ca_invalid")?])
            .build()
            .map_err(|_| "pool_client_invalid")?;
        Ok(Self {
            client,
            base: "https://kubernetes.default.svc".into(),
            token_file,
        })
    }
    pub async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, &'static str> {
        // Namespace/pool/node identifiers are validated before constructing paths.
        let token = std::fs::read_to_string(&self.token_file)
            .map_err(|_| "pool_kubernetes_token_required")?;
        let mut request = self
            .client
            .request(method, &format!("{}{}", self.base, path))
            .bearer_auth(token.trim());
        if let Some(body) = body {
            request = request.json(&body)
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| "pool_kubernetes_unavailable")?;
        if !response.status().is_success() {
            return Err("pool_kubernetes_rejected");
        }
        let mut bytes = Vec::new();
        while let Some(c) = response
            .chunk()
            .await
            .map_err(|_| "pool_kubernetes_unavailable")?
        {
            if bytes.len() + c.len() > 4 * 1024 * 1024 {
                return Err("pool_kubernetes_response_too_large");
            };
            bytes.extend_from_slice(&c);
        }
        serde_json::from_slice(&bytes).map_err(|_| "pool_kubernetes_response_invalid")
    }
}
fn identifier(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 63
        && v.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && v.as_bytes()[0].is_ascii_alphanumeric()
        && v.as_bytes()[v.len() - 1].is_ascii_alphanumeric()
}
impl PoolConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if [
            &self.namespace,
            &self.node_name,
            &self.pool,
            &self.runtime_class,
            &self.runtime_handler,
            &self.attestation_config_map,
        ]
        .iter()
        .any(|v| !identifier(v))
            || self.runtime_handler != "runsc"
            || !self.image.contains("@sha256:")
            || self
                .image
                .split("@sha256:")
                .nth(1)
                .is_none_or(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()))
            || self.warm == 0
            || self.warm > self.maximum
            || self.maximum > 32
            || !(600_000..=86_400_000).contains(&self.retention_ms)
            || !self.shared_root.is_absolute()
            || !self.socket.is_absolute()
            || !self.journal_root.is_absolute()
            || self.seccomp_profile.is_empty()
            || !self.cgroup_csi.is_object()
        {
            return Err("strong_pool_configuration_invalid");
        };
        Ok(())
    }
    fn pods_path(&self) -> String {
        format!("/api/v1/namespaces/{}/pods", self.namespace)
    }
    fn list_path(&self) -> String {
        format!(
            "{}?labelSelector=agentforge.io%2Fsandbox-pool%3D{}&fieldSelector=spec.nodeName%3D{}",
            self.pods_path(),
            self.pool,
            self.node_name
        )
    }
    pub(crate) fn pod(&self, name: &str) -> Value {
        let shared = self.shared_root.join(name).to_string_lossy().to_string();
        let mut args = vec![
            "-i".to_string(),
            "PATH=/usr/local/bin:/usr/bin:/bin".into(),
            "HOME=/tmp".into(),
            "LANG=C.UTF-8".into(),
            "ai-platform-python-sandbox".into(),
        ];
        for (key, value) in [
            ("--socket", "/run/slot/control.sock"),
            ("--public-key-id", self.public_key_id.as_str()),
            ("--public-key-hex", self.public_key_hex.as_str()),
            ("--required-runtime", "gvisor"),
            ("--runtime-evidence-file", "/run/slot/evidence.json"),
            ("--runtime-keyring-file", "/run/public/keys.json"),
            ("--await-runtime-evidence-ms", "120000"),
            ("--workspace-root", "/workspace"),
            ("--journal-root", "/journal"),
            ("--cgroup-root", "/sys/fs/cgroup/agentforge-sandbox"),
        ] {
            args.push(key.into());
            args.push(value.into());
        }
        json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":name,"namespace":self.namespace,"labels":{"agentforge.io/sandbox-pool":self.pool,"agentforge.io/sandbox-slot":name,"agentforge.io/sandbox-used":"false"}},"spec":{"nodeName":self.node_name,"runtimeClassName":self.runtime_class,"restartPolicy":"Never","automountServiceAccountToken":false,"enableServiceLinks":false,"hostNetwork":false,"hostPID":false,"hostIPC":false,"activeDeadlineSeconds":1800,"securityContext":{"runAsNonRoot":true,"runAsUser":10001,"runAsGroup":10001,"fsGroup":10001,"seccompProfile":{"type":"Localhost","localhostProfile":self.seccomp_profile}},"initContainers":[{"name":"measurement","image":self.image,"command":["/usr/bin/env"],"args":["-i","PATH=/usr/local/bin:/usr/bin:/bin","ai-platform-sandbox-measurement","/run/slot/request.json"],"securityContext":{"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true,"capabilities":{"drop":["ALL"]}},"resources":{"requests":{"cpu":"10m","memory":"16Mi"},"limits":{"cpu":"100m","memory":"32Mi"}},"volumeMounts":[{"name":"slot","mountPath":"/run/slot"}]}],"containers":[{"name":"sandbox","image":self.image,"command":["/usr/bin/env"],"args":args,"securityContext":{"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true,"capabilities":{"drop":["ALL"]}},"resources":{"requests":{"cpu":"100m","memory":"512Mi"},"limits":{"cpu":"1","memory":"1Gi"}},"readinessProbe":{"exec":{"command":["ai-platform-python-sandbox-health","/run/slot/control.sock"]},"periodSeconds":2,"timeoutSeconds":2},"volumeMounts":[{"name":"slot","mountPath":"/run/slot"},{"name":"public","mountPath":"/run/public","readOnly":true},{"name":"workspace","mountPath":"/workspace"},{"name":"journal","mountPath":"/journal"},{"name":"tmp","mountPath":"/tmp"},{"name":"cgroup","mountPath":"/sys/fs/cgroup/agentforge-sandbox"}]}],"volumes":[{"name":"slot","hostPath":{"path":shared,"type":"Directory"}},{"name":"public","configMap":{"name":self.attestation_config_map}},{"name":"workspace","emptyDir":{"medium":"Memory","sizeLimit":"64Mi"}},{"name":"journal","emptyDir":{"medium":"Memory","sizeLimit":"16Mi"}},{"name":"tmp","emptyDir":{"medium":"Memory","sizeLimit":"128Mi"}},{"name":"cgroup","csi":self.cgroup_csi}]}})
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Reservation {
    job_id: String,
    digest: String,
    slot: String,
    expires: u64,
}
enum Target {
    Socket(PathBuf),
    Cached(Vec<u8>),
}
const RECEIPT_BUDGET: u64 = 256 * 1024 * 1024;
struct PoolInner {
    reservations: BTreeMap<String, Reservation>,
}
#[derive(Clone)]
struct Pool {
    config: Arc<PoolConfig>,
    kube: Kube,
    verifier: Arc<SandboxJobVerifier>,
    inner: Arc<Mutex<PoolInner>>,
    _lock: Arc<std::fs::File>,
    io: Arc<tokio::sync::Semaphore>,
    dispatch_gates: Arc<Vec<Mutex<()>>>,
}
impl Pool {
    fn new(config: PoolConfig, kube: Kube) -> Result<Self, &'static str> {
        use std::os::unix::fs::OpenOptionsExt;
        config.validate()?;
        std::fs::create_dir_all(&config.journal_root).map_err(|_| "pool_journal_unavailable")?;
        std::fs::create_dir_all(&config.shared_root).map_err(|_| "pool_socket_root_unavailable")?;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .open(config.journal_root.join("controller.lock"))
            .map_err(|_| "pool_journal_unavailable")?;
        if unsafe {
            libc::flock(
                std::os::fd::AsRawFd::as_raw_fd(&lock),
                libc::LOCK_EX | libc::LOCK_NB,
            )
        } != 0
        {
            return Err("pool_controller_already_active");
        }
        let mut reservations = BTreeMap::new();
        if std::fs::read_dir(&config.journal_root)
            .map_err(|_| "pool_journal_unavailable")?
            .take(100001)
            .count()
            > 100000
        {
            return Err("pool_journal_capacity_exhausted");
        }
        for entry in
            std::fs::read_dir(&config.journal_root).map_err(|_| "pool_journal_unavailable")?
        {
            let path = entry.map_err(|_| "pool_journal_unavailable")?.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            let bytes = std::fs::read(path).map_err(|_| "pool_journal_unavailable")?;
            let r: Reservation =
                serde_json::from_slice(&bytes).map_err(|_| "pool_journal_corrupt")?;
            if !identifier(&r.slot) || uuid::Uuid::parse_str(&r.job_id).is_err() {
                return Err("pool_journal_corrupt");
            };
            reservations.insert(r.job_id.clone(), r);
        }
        if std::fs::read_dir(&config.shared_root)
            .map_err(|_| "pool_socket_root_unavailable")?
            .take(100001)
            .count()
            > 100000
        {
            return Err("pool_socket_capacity_exhausted");
        }
        let key = protocol_key(&config.public_key_hex)?;
        let verifier =
            SandboxJobVerifier::new(BTreeMap::from([(config.public_key_id.clone(), key)]))?;
        Ok(Self {
            config: Arc::new(config),
            kube,
            verifier: Arc::new(verifier),
            inner: Arc::new(Mutex::new(PoolInner { reservations })),
            _lock: Arc::new(lock),
            io: Arc::new(tokio::sync::Semaphore::new(2)),
            dispatch_gates: Arc::new((0..64).map(|_| Mutex::new(())).collect()),
        })
    }
    async fn pods(&self) -> Result<Vec<Value>, &'static str> {
        let v = self
            .kube
            .request(reqwest::Method::GET, &self.config.list_path(), None)
            .await?;
        Ok(v["items"].as_array().ok_or("pool_pods_invalid")?.clone())
    }
    async fn reconcile(&self) -> Result<(), &'static str> {
        let class = self
            .kube
            .request(
                reqwest::Method::GET,
                &format!(
                    "/apis/node.k8s.io/v1/runtimeclasses/{}",
                    self.config.runtime_class
                ),
                None,
            )
            .await?;
        if class["handler"] != self.config.runtime_handler {
            return Err("pool_runtimeclass_not_admitted");
        }
        let pods = self.pods().await?;
        if std::fs::read_dir(&self.config.shared_root)
            .map_err(|_| "pool_socket_root_unavailable")?
            .take(100001)
            .count()
            > 100000
        {
            return Err("pool_socket_capacity_exhausted");
        }
        let now = crate::now_epoch_ms();
        let inner = self.inner.lock().await;
        let mut unused = 0;
        let mut count = 0;
        for pod in &pods {
            let name = pod["metadata"]["name"].as_str().ok_or("pool_pod_invalid")?;
            let created = pod["metadata"]["creationTimestamp"]
                .as_str()
                .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
                .map(|v| v.timestamp_millis().max(0) as u64)
                .unwrap_or(now);
            let assigned = inner.reservations.values().find(|r| r.slot == name);
            let expired = assigned
                .map(|r| now > r.expires.saturating_add(self.config.retention_ms))
                .unwrap_or(now > created.saturating_add(300_000));
            let completed = assigned.is_some_and(|r| {
                self.config
                    .journal_root
                    .join(format!("{}.receipt", r.job_id))
                    .is_file()
            });
            if !pod["metadata"]["deletionTimestamp"].is_null() {
                count += 1;
                continue;
            }
            let cancelled = assigned.is_some_and(|r| {
                self.config
                    .journal_root
                    .join(format!("{}.cancel", r.job_id))
                    .is_file()
            });
            if expired
                || cancelled
                || completed
                || matches!(
                    pod["status"]["phase"].as_str(),
                    Some("Failed" | "Succeeded")
                ) && assigned.is_none()
            {
                self.kube.request(reqwest::Method::DELETE,&format!("{}/{}",self.config.pods_path(),name),Some(json!({"apiVersion":"v1","kind":"DeleteOptions","preconditions":{"uid":pod["metadata"]["uid"]},"gracePeriodSeconds":5}))).await?;
                count += 1;
            } else {
                count += 1;
                if assigned.is_none() {
                    unused += 1;
                }
            }
        }
        for r in inner
            .reservations
            .values()
            .filter(|r| now > r.expires.saturating_add(self.config.retention_ms))
        {
            for suffix in ["receipt", "pending"] {
                let path = self
                    .config
                    .journal_root
                    .join(format!("{}.{suffix}", r.job_id));
                match std::fs::remove_file(path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err("pool_receipt_expiry_failed"),
                }
            }
        }
        // Historical reservations remain tombstones. Reusing an execution ID
        // never launches a fresh sandbox, even after Kubernetes deletes its pod.
        if inner.reservations.len() >= 100_000 {
            return Err("pool_journal_capacity_exhausted");
        }
        for _ in unused..self.config.warm {
            if count >= self.config.maximum {
                break;
            }
            let name = format!("slot-{}", uuid::Uuid::now_v7().simple());
            let path = self.config.shared_root.join(&name);
            std::fs::create_dir(&path).map_err(|_| "pool_slot_create_failed")?;
            // Only this slot is mounted in the untrusted pod; never its parent.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o770))
                    .map_err(|_| "pool_slot_permissions_failed")?;
            }
            self.kube
                .request(
                    reqwest::Method::POST,
                    &self.config.pods_path(),
                    Some(self.config.pod(&name)),
                )
                .await?;
            count += 1;
        }
        Ok(())
    }
    async fn route(&self, sealed: &SealedSandboxMessage) -> Result<Target, &'static str> {
        let msg = self.verifier.verify(sealed, crate::now_epoch_ms())?;
        let (id, digest, expires) = match msg {
            SandboxMessage::Execute(job) => {
                (job.job_id.clone(), job.digest()?, job.expires_at_epoch_ms)
            }
            SandboxMessage::Poll(a) | SandboxMessage::Cancel(a) => (
                a.job_id.clone(),
                a.job_digest.clone(),
                a.expires_at_epoch_ms,
            ),
        };
        uuid::Uuid::parse_str(&id).map_err(|_| "pool_job_id_invalid")?;
        let cancelled = matches!(msg, SandboxMessage::Execute(_))
            && self
                .config
                .journal_root
                .join(format!("{id}.cancel"))
                .exists();
        let mut inner = self.inner.lock().await;
        if let Some(r) = inner.reservations.get(&id) {
            if r.digest != digest {
                return Err("pool_job_conflict");
            };
            if let Some(bytes) = self.receipt(&r.job_id)? {
                return Ok(Target::Cached(bytes));
            }
            if cancelled {
                return Err("sandbox_cancellation_requested");
            }
            return Ok(Target::Socket(
                self.config.shared_root.join(&r.slot).join("control.sock"),
            ));
        }
        if cancelled {
            return Err("sandbox_cancellation_requested");
        }
        if !matches!(msg, SandboxMessage::Execute(_)) {
            return Err("pool_job_not_found");
        }
        let mut occupied = 0u64;
        for entry in
            std::fs::read_dir(&self.config.journal_root).map_err(|_| "pool_journal_unavailable")?
        {
            let entry = entry.map_err(|_| "pool_journal_unavailable")?;
            if matches!(
                entry.path().extension().and_then(|s| s.to_str()),
                Some("receipt" | "pending")
            ) {
                occupied = occupied.saturating_add(
                    entry
                        .metadata()
                        .map_err(|_| "pool_journal_unavailable")?
                        .len(),
                );
            }
        }
        occupied = occupied.saturating_add(
            inner
                .reservations
                .values()
                .filter(|r| {
                    r.expires.saturating_add(self.config.retention_ms) > crate::now_epoch_ms()
                        && !self
                            .config
                            .journal_root
                            .join(format!("{}.receipt", r.job_id))
                            .exists()
                })
                .count() as u64
                * MAX_SANDBOX_WIRE_BYTES as u64,
        );
        if occupied.saturating_add(MAX_SANDBOX_WIRE_BYTES as u64) > RECEIPT_BUDGET {
            return Err("capacity_exhausted");
        }
        let pods = self.pods().await?;
        let pod = pods
            .iter()
            .find(|p| {
                let name = p["metadata"]["name"].as_str().unwrap_or("");
                identifier(name)
                    && p["metadata"]["deletionTimestamp"].is_null()
                    && p["spec"]["runtimeClassName"] == self.config.runtime_class
                    && p["spec"]["nodeName"] == self.config.node_name
                    && p["status"]["conditions"].as_array().is_some_and(|a| {
                        a.iter()
                            .any(|c| c["type"] == "Ready" && c["status"] == "True")
                    })
                    && !inner.reservations.values().any(|r| r.slot == name)
            })
            .ok_or("capacity_exhausted")?;
        let slot = pod["metadata"]["name"]
            .as_str()
            .ok_or("pool_pod_invalid")?
            .to_owned();
        let r = Reservation {
            job_id: id.clone(),
            digest,
            slot: slot.clone(),
            expires,
        };
        persist(&self.config.journal_root.join(format!("{id}.json")), &r)?;
        inner.reservations.insert(id, r);
        Ok(Target::Socket(
            self.config.shared_root.join(slot).join("control.sock"),
        ))
    }
    fn receipt(&self, id: &str) -> Result<Option<Vec<u8>>, &'static str> {
        use std::io::Read;
        let path = self.config.journal_root.join(format!("{id}.receipt"));
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("pool_receipt_unavailable"),
        };
        if !meta.is_file() || meta.len() > MAX_SANDBOX_WIRE_BYTES as u64 {
            return Err("pool_receipt_invalid");
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|_| "pool_receipt_unavailable")?
            .take(MAX_SANDBOX_WIRE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "pool_receipt_unavailable")?;
        if bytes.len() > MAX_SANDBOX_WIRE_BYTES {
            return Err("pool_receipt_invalid");
        }
        let reply: crate::sandbox_transport::SandboxReply =
            serde_json::from_slice(&bytes).map_err(|_| "pool_receipt_invalid")?;
        if reply.job_id != id
            || matches!(
                reply.state,
                crate::sandbox_transport::SandboxReplyState::Running
            )
        {
            return Err("pool_receipt_invalid");
        };
        Ok(Some(bytes))
    }
    fn save_cancel(&self, id: &str) -> Result<(), &'static str> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        uuid::Uuid::parse_str(id).map_err(|_| "pool_job_id_invalid")?;
        let intent = self.config.journal_root.join(format!("{id}.cancel"));
        if intent.exists() {
            return Ok(());
        }
        if std::fs::read_dir(&self.config.journal_root)
            .map_err(|_| "pool_journal_unavailable")?
            .take(100001)
            .count()
            >= 100000
        {
            return Err("pool_journal_capacity_exhausted");
        }
        let saved = (|| -> std::io::Result<()> {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&intent)?;
            f.write_all(b"cancel_requested")?;
            f.sync_all()?;
            std::fs::File::open(&self.config.journal_root)?.sync_all()
        })();
        saved.map_err(|_| "pool_cancel_acknowledgement_unknown")
    }
    async fn cache_final(
        &self,
        sealed: &SealedSandboxMessage,
        bytes: &[u8],
    ) -> Result<(), &'static str> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let reply: crate::sandbox_transport::SandboxReply =
            serde_json::from_slice(bytes).map_err(|_| "pool_response_invalid")?;
        let id = match &sealed.message {
            SandboxMessage::Execute(j) => &j.job_id,
            SandboxMessage::Poll(a) | SandboxMessage::Cancel(a) => &a.job_id,
        };
        let inner = self.inner.lock().await;
        let r = inner.reservations.get(id).ok_or("pool_job_not_found")?;
        if reply.job_id != *id || reply.job_digest != r.digest {
            return Err("pool_response_binding_invalid");
        }
        if matches!(
            reply.state,
            crate::sandbox_transport::SandboxReplyState::Running
        ) {
            return Ok(());
        }
        if let Some(prior) = self.receipt(id)? {
            return if prior == bytes {
                Ok(())
            } else {
                Err("pool_terminal_receipt_conflict")
            };
        }
        let pending = self.config.journal_root.join(format!("{id}.pending"));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&pending)
            .map_err(|_| "pool_receipt_write_unknown")?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "pool_receipt_write_unknown")?;
        std::fs::rename(
            pending,
            self.config.journal_root.join(format!("{id}.receipt")),
        )
        .map_err(|_| "pool_receipt_write_unknown")?;
        std::fs::File::open(&self.config.journal_root)
            .and_then(|f| f.sync_all())
            .map_err(|_| "pool_receipt_write_unknown")?;
        // Reconcile deletes this single-use pod only after its terminal receipt
        // is durable. Dropped responses replay these exact bytes after restart.
        Ok(())
    }
}
fn protocol_key(s: &str) -> Result<[u8; 32], &'static str> {
    let bytes = hex::decode(s).map_err(|_| "pool_public_key_invalid")?;
    bytes.try_into().map_err(|_| "pool_public_key_invalid")
}
fn persist(path: &Path, r: &Reservation) -> Result<(), &'static str> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| "pool_reservation_unavailable")?;
    f.write_all(&serde_json::to_vec(r).map_err(|_| "pool_reservation_invalid")?)
        .and_then(|_| f.sync_all())
        .map_err(|_| "pool_reservation_unavailable")?;
    std::fs::File::open(path.parent().ok_or("pool_journal_invalid")?)
        .and_then(|f| f.sync_all())
        .map_err(|_| "pool_reservation_unavailable")
}
async fn admit_io(
    State(pool): State<Pool>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let Ok(_permit) = pool.io.clone().try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"capacity_exhausted"})),
        )
            .into_response();
    };
    next.run(request).await
}
async fn dispatch(State(pool): State<Pool>, Json(sealed): Json<SealedSandboxMessage>) -> Response {
    if pool
        .verifier
        .verify(&sealed, crate::now_epoch_ms())
        .is_err()
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    // Serialize the initial execute acknowledgement and cancellation per job.
    // A durable cancellation arriving first prevents the later initial send;
    // a cancellation arriving second reaches an already admitted sandbox job.
    let id = match &sealed.message {
        SandboxMessage::Execute(j) => &j.job_id,
        SandboxMessage::Poll(a) | SandboxMessage::Cancel(a) => &a.job_id,
    };
    if uuid::Uuid::parse_str(id).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let bucket = id
        .bytes()
        .fold(0usize, |n, b| (n.wrapping_mul(31) + b as usize) % 64);
    let _dispatch = pool.dispatch_gates[bucket].lock().await;
    if matches!(sealed.message, SandboxMessage::Cancel(_)) && pool.save_cancel(id).is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let socket = match pool.route(&sealed).await {
        Ok(Target::Cached(bytes)) => {
            return (
                StatusCode::OK,
                [("content-type", "application/json")],
                bytes,
            )
                .into_response();
        }
        Ok(Target::Socket(p)) => p,
        Err(e) => {
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":e}))).into_response();
        }
    };
    let client = match reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .unix_socket(socket)
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let mut response = match client
        .post("http://sandbox.invalid/internal/v1/jobs")
        .json(&sealed)
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error":"sandbox_acknowledgement_unknown"})),
            )
                .into_response();
        }
    };
    let status = response.status();
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(c)) => {
                if bytes.len() + c.len() > MAX_SANDBOX_WIRE_BYTES {
                    return StatusCode::BAD_GATEWAY.into_response();
                };
                bytes.extend_from_slice(&c)
            }
            Ok(None) => break,
            Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
        }
    }
    if status.is_success() && pool.cache_final(&sealed, &bytes).await.is_err() {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error":"sandbox_receipt_acknowledgement_unknown"})),
        )
            .into_response();
    }
    (status, [("content-type", "application/json")], bytes).into_response()
}
pub async fn serve(config: PoolConfig) -> Result<(), &'static str> {
    let socket = config.socket.clone();
    let pool = Pool::new(config, Kube::in_cluster()?)?;
    pool.reconcile().await?;
    let background = pool.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if let Err(error) = background.reconcile().await {
                tracing::warn!(error, "strong sandbox pool reconciliation failed");
            }
        }
    });
    // Refuse to unlink an arbitrary preexisting filesystem object.
    if let Ok(meta) = std::fs::symlink_metadata(&socket) {
        use std::os::unix::fs::FileTypeExt;
        if !meta.file_type().is_socket() {
            return Err("pool_socket_unsafe");
        };
        std::fs::remove_file(&socket).map_err(|_| "pool_socket_unavailable")?;
    }
    let listener =
        tokio::net::UnixListener::bind(&socket).map_err(|_| "pool_socket_unavailable")?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o660))
            .map_err(|_| "pool_socket_permissions_failed")?;
    }
    let router = Router::new()
        .route(
            "/health/ready",
            get(|State(p): State<Pool>| async move {
                match p.pods().await {
                    Ok(pods)
                        if pods.iter().any(|v| {
                            v["status"]["conditions"].as_array().is_some_and(|a| {
                                a.iter()
                                    .any(|c| c["type"] == "Ready" && c["status"] == "True")
                            })
                        }) =>
                    {
                        StatusCode::OK
                    }
                    _ => StatusCode::SERVICE_UNAVAILABLE,
                }
            }),
        )
        .route("/internal/v1/jobs", post(dispatch))
        .layer(DefaultBodyLimit::max(MAX_SANDBOX_WIRE_BYTES))
        .layer(axum::middleware::from_fn_with_state(pool.clone(), admit_io))
        .with_state(pool);
    axum::serve(listener, router)
        .await
        .map_err(|_| "pool_listener_failed")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_cannot_escape_kubernetes_or_socket_paths() {
        for v in ["", "../slot", "UPPER", "-bad", "bad/slot", "slot?x=1"] {
            assert!(!identifier(v))
        }
        assert!(identifier("slot-abc-123"));
    }
    #[tokio::test]
    async fn terminal_unknown_receipt_survives_restart_without_reassigning_or_dispatching() {
        use ai_platform_capability_contract::{
            CapabilityScopeV2,
            sandbox_job::{SANDBOX_JOB_SCHEMA, SandboxJob, SandboxJobSigner},
        };
        let root =
            std::env::temp_dir().join(format!("pool-restart-fixture-{}", uuid::Uuid::now_v7()));
        let signer = SandboxJobSigner::new("fixture".into(), [7; 32]).unwrap();
        let now = crate::now_epoch_ms();
        let config:PoolConfig=serde_json::from_value(json!({"namespace":"sandbox","node_name":"node","pool":"strong","runtime_class":"gvisor","runtime_handler":"runsc","image":format!("sandbox@sha256:{}","a".repeat(64)),"shared_root":root.join("slots"),"socket":root.join("control.sock"),"journal_root":root.join("journal"),"warm":1,"maximum":2,"retention_ms":600000,"public_key_id":"fixture","public_key_hex":hex::encode(signer.public_key_bytes()),"attestation_config_map":"public","seccomp_profile":"sandbox.json","cgroup_csi":{"driver":"fixture.cgroup"}})).unwrap();
        let job = SandboxJob {
            schema_version: SANDBOX_JOB_SCHEMA.into(),
            job_id: uuid::Uuid::now_v7().to_string(),
            scope: CapabilityScopeV2 {
                tenant_id: "tenant".into(),
                user_id: "user".into(),
                session_id: "session".into(),
            },
            run_id: "run".into(),
            tool_call_id: "call".into(),
            arguments_hash: format!("sha256:{}", "a".repeat(64)),
            issued_at_epoch_ms: now,
            expires_at_epoch_ms: now + 100000,
            code: "print(1)".into(),
            inputs: json!([]),
            limits: json!({}),
        };
        std::fs::create_dir_all(&config.journal_root).unwrap();
        let r = Reservation {
            job_id: job.job_id.clone(),
            digest: job.digest().unwrap(),
            slot: "slot-original".into(),
            expires: job.expires_at_epoch_ms,
        };
        persist(
            &config.journal_root.join(format!("{}.json", job.job_id)),
            &r,
        )
        .unwrap();
        let kube = Kube {
            client: reqwest::Client::new(),
            base: "https://never-dispatched.invalid".into(),
            token_file: root.join("missing-token"),
        };
        let cancelled_pool = Pool::new(config.clone(), kube.clone()).unwrap();
        let mut delayed = job.clone();
        delayed.job_id = uuid::Uuid::now_v7().to_string();
        cancelled_pool.save_cancel(&delayed.job_id).unwrap();
        drop(cancelled_pool);
        let cancelled_pool = Pool::new(config.clone(), kube.clone()).unwrap();
        let delayed = signer.seal(SandboxMessage::Execute(delayed), now).unwrap();
        assert!(matches!(
            cancelled_pool.route(&delayed).await,
            Err("sandbox_cancellation_requested")
        ));
        assert!(cancelled_pool.save_cancel("../escape").is_err());
        drop(cancelled_pool);
        let sealed = signer
            .seal(SandboxMessage::Execute(job.clone()), now)
            .unwrap();
        let bytes = serde_json::to_vec(&crate::sandbox_transport::SandboxReply {
            job_id: job.job_id.clone(),
            job_digest: job.digest().unwrap(),
            state: crate::sandbox_transport::SandboxReplyState::SideEffectUnknown,
        })
        .unwrap();
        let pool = Pool::new(config.clone(), kube.clone()).unwrap();
        pool.cache_final(&sealed, &bytes).await.unwrap();
        drop(pool);
        let restarted = Pool::new(config, kube).unwrap();
        match restarted.route(&sealed).await.unwrap() {
            Target::Cached(actual) => assert_eq!(actual, bytes),
            Target::Socket(_) => panic!("terminal receipt was dispatched again"),
        };
        let mut altered = job;
        altered.code = "print(2)".into();
        let forged = signer.seal(SandboxMessage::Execute(altered), now).unwrap();
        assert!(restarted.route(&forged).await.is_err());
        drop(restarted);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn durable_reservation_cannot_be_overwritten() {
        let root = std::env::temp_dir().join(format!("pool-fixture-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let p = root.join("a.json");
        let r = Reservation {
            job_id: uuid::Uuid::now_v7().to_string(),
            digest: "digest".into(),
            slot: "slot-1".into(),
            expires: 10,
        };
        persist(&p, &r).unwrap();
        assert!(persist(&p, &r).is_err());
        assert_eq!(
            serde_json::from_slice::<Reservation>(&std::fs::read(p).unwrap())
                .unwrap()
                .digest,
            "digest"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
