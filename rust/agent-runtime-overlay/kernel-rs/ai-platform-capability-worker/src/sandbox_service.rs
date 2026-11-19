//! Dedicated credential-free execution service. The production constructor
//! rejects a control-plane environment; the binary listens only on a Unix socket.
use crate::python_code_execution::{
    CodeExecutionError, LocalPythonSandboxBroker, LocalPythonSandboxConfig,
    PythonCodeExecutionRequest, PythonCodeExecutionResult, validate_request,
};
use crate::sandbox_transport::{MAX_SANDBOX_WIRE_BYTES, SandboxReply, SandboxReplyState};
use ai_platform_capability_contract::{
    CapabilityEffect, CapabilityScopeV2, RuntimeCapabilityLeaseV1,
    sandbox_job::{
        SandboxAccess, SandboxJob, SandboxJobVerifier, SandboxMessage, SealedSandboxMessage,
    },
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Mutex, Semaphore};

#[async_trait::async_trait]
trait SandboxExecutor: Send + Sync {
    fn available(&self) -> bool;
    async fn execute(
        &self,
        request: PythonCodeExecutionRequest,
        cancel: Arc<AtomicBool>,
    ) -> Result<PythonCodeExecutionResult, CodeExecutionError>;
}
#[async_trait::async_trait]
impl SandboxExecutor for LocalPythonSandboxBroker {
    fn available(&self) -> bool {
        self.isolation_available()
    }
    async fn execute(
        &self,
        request: PythonCodeExecutionRequest,
        cancel: Arc<AtomicBool>,
    ) -> Result<PythonCodeExecutionResult, CodeExecutionError> {
        self.execute_cancellable(request, cancel).await
    }
}

struct TenantExecutor {
    base: LocalPythonSandboxBroker,
    config: LocalPythonSandboxConfig,
    policy: Option<crate::python_package_policy::TenantPackagePolicy>,
    strong: Option<Arc<crate::sandbox_tier::StrongTierAdmission>>,
}
#[async_trait::async_trait]
impl SandboxExecutor for TenantExecutor {
    fn available(&self) -> bool {
        self.base.isolation_available()
            && self
                .strong
                .as_ref()
                .is_none_or(|admission| admission.available())
    }
    async fn execute(
        &self,
        request: PythonCodeExecutionRequest,
        cancel: Arc<AtomicBool>,
    ) -> Result<PythonCodeExecutionResult, CodeExecutionError> {
        if !self.available() {
            return Err(CodeExecutionError::IsolationUnavailable);
        }
        let mut config = self.config.clone();
        config.package_roots = match &self.policy {
            Some(policy) => policy
                .paths_for(&request.lease.tenant_id)
                .map_err(|_| CodeExecutionError::Configuration)?,
            None => Vec::new(),
        };
        LocalPythonSandboxBroker::new(config)?
            .execute_cancellable(request, cancel)
            .await
    }
}

#[derive(Clone, Debug)]
pub struct SandboxServiceConfig {
    pub local: LocalPythonSandboxConfig,
    pub journal_root: PathBuf,
    pub package_policy: Option<PathBuf>,
    pub strong_tier: Option<crate::sandbox_tier::StrongTierConfig>,
    pub concurrency: usize,
    pub max_records: usize,
    pub retention_ms: u64,
}
impl Default for SandboxServiceConfig {
    fn default() -> Self {
        Self {
            local: Default::default(),
            journal_root: "/var/lib/agentforge-sandbox/jobs".into(),
            package_policy: None,
            strong_tier: None,
            concurrency: 2,
            max_records: 1024,
            retention_ms: 3_600_000,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum JournalState {
    Running,
    Finished,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalRecord {
    job_id: String,
    scope: CapabilityScopeV2,
    digest: String,
    expires: u64,
    state: JournalState,
}
struct Entry {
    record: JournalRecord,
    cancel: Arc<AtomicBool>,
}
#[derive(Clone)]
pub struct SandboxService {
    executor: Arc<dyn SandboxExecutor>,
    verifier: Arc<SandboxJobVerifier>,
    root: Arc<PathBuf>,
    entries: Arc<Mutex<BTreeMap<String, Entry>>>,
    slots: Arc<Semaphore>,
    max_records: usize,
    retention_ms: u64,
    single_use: bool,
    _journal_lock: Arc<File>,
}
#[derive(Debug, Serialize)]
struct ServiceError {
    error: &'static str,
}
type Failure = (StatusCode, Json<ServiceError>);
fn failure(status: StatusCode, error: &'static str) -> Failure {
    (status, Json(ServiceError { error }))
}
fn canonical_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|value| value.to_string() == id)
}

impl SandboxService {
    pub fn new(
        config: SandboxServiceConfig,
        verifier: SandboxJobVerifier,
    ) -> Result<Self, &'static str> {
        credential_free_preflight()?;
        crate::python_package_policy::verify_python_runtime(&config.local.python_binary)?;
        let policy = config
            .package_policy
            .as_ref()
            .map(|path| {
                crate::python_package_policy::TenantPackagePolicy::load(
                    path,
                    &config.local.python_binary,
                )
            })
            .transpose()?;
        let strong = config
            .strong_tier
            .as_ref()
            .map(crate::sandbox_tier::StrongTierAdmission::load)
            .transpose()?
            .map(Arc::new);
        let executor = Arc::new(TenantExecutor {
            base: LocalPythonSandboxBroker::new(config.local.clone())
                .map_err(|_| "sandbox_config_invalid")?,
            config: config.local.clone(),
            policy,
            strong,
        });
        Self::from_parts(config, verifier, executor)
    }
    fn from_parts(
        config: SandboxServiceConfig,
        verifier: SandboxJobVerifier,
        executor: Arc<dyn SandboxExecutor>,
    ) -> Result<Self, &'static str> {
        if config.strong_tier.is_some() && (config.concurrency != 1 || config.max_records != 1) {
            return Err("strong_sandbox_requires_one_execution");
        }
        if !config.journal_root.is_absolute()
            || config.concurrency == 0
            || config.concurrency > 32
            || config.max_records < config.concurrency
            || config.max_records > 4096
            || config.retention_ms < 360_000
            || config.retention_ms > 86_400_000
        {
            return Err("sandbox_config_invalid");
        }
        private_directory(&config.journal_root).map_err(|_| "sandbox_journal_unavailable")?;
        let lock_path = config.journal_root.join(".service.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let journal_lock = options
            .open(lock_path)
            .map_err(|_| "sandbox_journal_lock_failed")?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if unsafe { libc::flock(journal_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0
            {
                return Err("sandbox_journal_already_owned");
            }
        }
        let mut entries = BTreeMap::new();
        for entry in
            fs::read_dir(&config.journal_root).map_err(|_| "sandbox_journal_unavailable")?
        {
            let entry = entry.map_err(|_| "sandbox_journal_unavailable")?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".meta.json") {
                continue;
            }
            if entries.len() >= config.max_records {
                return Err("sandbox_journal_capacity_exhausted");
            }
            let mut record: JournalRecord =
                read_bounded(&entry.path(), 16 * 1024).map_err(|_| "sandbox_journal_invalid")?;
            if !canonical_id(&record.job_id) || name != format!("{}.meta.json", record.job_id) {
                return Err("sandbox_journal_invalid");
            }
            if record.state == JournalState::Running {
                // A fully fsynced result can reconcile a crash before its marker.
                record.state =
                    if read_bounded::<Result<PythonCodeExecutionResult, CodeExecutionError>>(
                        &result_path(&config.journal_root, &record.job_id),
                        MAX_SANDBOX_WIRE_BYTES,
                    )
                    .is_ok()
                    {
                        JournalState::Finished
                    } else {
                        JournalState::Unknown
                    };
                atomic_json(
                    &config.journal_root,
                    &meta_path(&config.journal_root, &record.job_id),
                    &record,
                )
                .map_err(|_| "sandbox_journal_unavailable")?;
            }
            entries.insert(
                record.job_id.clone(),
                Entry {
                    record,
                    cancel: Arc::new(AtomicBool::new(false)),
                },
            );
        }
        Ok(Self {
            executor,
            verifier: Arc::new(verifier),
            root: Arc::new(config.journal_root),
            entries: Arc::new(Mutex::new(entries)),
            slots: Arc::new(Semaphore::new(config.concurrency)),
            max_records: config.max_records,
            retention_ms: config.retention_ms,
            single_use: config.strong_tier.is_some(),
            _journal_lock: Arc::new(journal_lock),
        })
    }
    pub fn router(self) -> Router {
        Router::new()
            .route("/health/ready", get(ready))
            .route("/internal/v1/jobs", post(handle))
            .layer(DefaultBodyLimit::max(MAX_SANDBOX_WIRE_BYTES))
            .with_state(self)
    }
    async fn reply(&self, record: &JournalRecord) -> Result<SandboxReply, Failure> {
        let state = match record.state {
            JournalState::Running => SandboxReplyState::Running,
            JournalState::Unknown => SandboxReplyState::SideEffectUnknown,
            JournalState::Finished => {
                let path = result_path(&self.root, &record.job_id);
                let result = tokio::task::spawn_blocking(move || {
                    read_bounded(&path, MAX_SANDBOX_WIRE_BYTES)
                })
                .await
                .map_err(|_| {
                    failure(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "sandbox_receipt_unavailable",
                    )
                })?
                .map_err(|_| {
                    failure(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "sandbox_receipt_unavailable",
                    )
                })?;
                SandboxReplyState::Completed(result)
            }
        };
        Ok(SandboxReply {
            job_id: record.job_id.clone(),
            job_digest: record.digest.clone(),
            state,
        })
    }
    async fn access(&self, access: SandboxAccess, cancel: bool) -> Result<SandboxReply, Failure> {
        let entries = self.entries.lock().await;
        let entry = entries
            .get(&access.job_id)
            .filter(|entry| entry.record.scope == access.scope)
            .ok_or_else(|| failure(StatusCode::NOT_FOUND, "sandbox_job_not_found"))?;
        if entry.record.digest != access.job_digest {
            return Err(failure(StatusCode::CONFLICT, "sandbox_job_conflict"));
        }
        if cancel && entry.record.state == JournalState::Running {
            entry.cancel.store(true, Ordering::Release);
        }
        let record = entry.record.clone();
        drop(entries);
        self.reply(&record).await
    }
    async fn submit(&self, job: SandboxJob) -> Result<SandboxReply, Failure> {
        if !canonical_id(&job.job_id) {
            return Err(failure(StatusCode::BAD_REQUEST, "sandbox_job_invalid"));
        }
        let digest = job
            .digest()
            .map_err(|_| failure(StatusCode::BAD_REQUEST, "sandbox_job_invalid"))?;
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.get(&job.job_id) {
            if entry.record.scope != job.scope {
                return Err(failure(StatusCode::NOT_FOUND, "sandbox_job_not_found"));
            }
            if entry.record.digest != digest {
                return Err(failure(StatusCode::CONFLICT, "sandbox_job_conflict"));
            }
            let record = entry.record.clone();
            drop(entries);
            return self.reply(&record).await;
        }
        if self.single_use && !entries.is_empty() {
            return Err(failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "strong_sandbox_already_used",
            ));
        }
        let request = local_request(&job)
            .map_err(|_| failure(StatusCode::BAD_REQUEST, "sandbox_job_invalid"))?;
        if !self.executor.available() {
            return Err(failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "sandbox_isolation_unavailable",
            ));
        }
        // Prune only expired terminal receipts, never running work. An original
        // expired sealed job cannot be replayed after pruning.
        let now = crate::now_epoch_ms();
        let expired: Vec<_> = entries
            .iter()
            .filter(|(_, entry)| {
                entry.record.state != JournalState::Running
                    && entry.record.expires.saturating_add(self.retention_ms) < now
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            fs::remove_file(meta_path(&self.root, &id)).map_err(|_| {
                failure(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "sandbox_journal_unavailable",
                )
            })?;
            let _ = fs::remove_file(result_path(&self.root, &id));
            entries.remove(&id);
        }
        if entries.len() >= self.max_records {
            return Err(failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "capacity_exhausted",
            ));
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| failure(StatusCode::SERVICE_UNAVAILABLE, "capacity_exhausted"))?;
        let record = JournalRecord {
            job_id: job.job_id,
            scope: job.scope,
            digest,
            expires: job.expires_at_epoch_ms,
            state: JournalState::Running,
        };
        let root = self.root.clone();
        let initial = record.clone();
        tokio::task::spawn_blocking(move || {
            atomic_json(&root, &meta_path(&root, &initial.job_id), &initial)
        })
        .await
        .map_err(|_| {
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "sandbox_journal_unavailable",
            )
        })?
        .map_err(|_| {
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "sandbox_journal_unavailable",
            )
        })?;
        let cancel = Arc::new(AtomicBool::new(false));
        entries.insert(
            record.job_id.clone(),
            Entry {
                record: record.clone(),
                cancel: cancel.clone(),
            },
        );
        drop(entries);
        let service = self.clone();
        let running = record.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let outcome = service.executor.execute(request, cancel).await;
            let root = service.root.clone();
            let mut finished = running.clone();
            finished.state = JournalState::Finished;
            let to_save = finished.clone();
            let saved = tokio::task::spawn_blocking(move || {
                atomic_json(&root, &result_path(&root, &to_save.job_id), &outcome)?;
                atomic_json(&root, &meta_path(&root, &to_save.job_id), &to_save)
            })
            .await;
            if !matches!(saved, Ok(Ok(()))) {
                finished.state = JournalState::Unknown;
            }
            if let Some(entry) = service.entries.lock().await.get_mut(&finished.job_id) {
                entry.record = finished;
            }
        });
        self.reply(&record).await
    }
}
async fn ready(State(service): State<SandboxService>) -> StatusCode {
    if service.executor.available()
        && (!service.single_use || service.entries.lock().await.is_empty())
    {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
async fn handle(
    State(service): State<SandboxService>,
    Json(sealed): Json<SealedSandboxMessage>,
) -> Result<Json<SandboxReply>, Failure> {
    service
        .verifier
        .verify(&sealed, crate::now_epoch_ms())
        .map_err(|_| failure(StatusCode::FORBIDDEN, "sandbox_seal_invalid"))?;
    let result = match sealed.message {
        SandboxMessage::Execute(job) => service.submit(job).await?,
        SandboxMessage::Poll(access) => service.access(access, false).await?,
        SandboxMessage::Cancel(access) => service.access(access, true).await?,
    };
    Ok(Json(result))
}
fn local_request(job: &SandboxJob) -> Result<PythonCodeExecutionRequest, CodeExecutionError> {
    // Synthetic local metadata is not a platform lease: no signature, approval
    // token, bearer secret or live runtime authority is forwarded to the sandbox.
    let request = PythonCodeExecutionRequest {
        lease: RuntimeCapabilityLeaseV1 {
            schema_version:
                ai_platform_capability_contract::RUNTIME_CAPABILITY_LEASE_SCHEMA_VERSION.into(),
            lease_id: job.job_id.clone(),
            tenant_id: job.scope.tenant_id.clone(),
            user_id: job.scope.user_id.clone(),
            session_id: job.scope.session_id.clone(),
            run_id: job.run_id.clone(),
            tool_call_id: job.tool_call_id.clone(),
            attempt_id: "sealed-sandbox".into(),
            capability_id: "execute_python_code".into(),
            capability_revision: 1,
            arguments_hash: job.arguments_hash.clone(),
            effect: CapabilityEffect::Write,
            approval_id: None,
            issued_at_epoch_ms: job.issued_at_epoch_ms,
            expires_at_epoch_ms: job.expires_at_epoch_ms,
            nonce: job.job_id.clone(),
            signature: String::new(),
        },
        arguments_hash: job.arguments_hash.clone(),
        code: job.code.clone(),
        inputs: serde_json::from_value(job.inputs.clone())
            .map_err(|_| CodeExecutionError::Configuration)?,
        limits: serde_json::from_value(job.limits.clone())
            .map_err(|_| CodeExecutionError::Configuration)?,
    };
    validate_request(&request)?;
    Ok(request)
}
fn meta_path(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{id}.meta.json"))
}
fn result_path(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{id}.result.json"))
}
fn read_bounded<T: serde::de::DeserializeOwned>(path: &Path, maximum: usize) -> std::io::Result<T> {
    use std::io::Read;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > maximum as u64 {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    serde_json::from_slice(&bytes).map_err(|_| std::io::ErrorKind::InvalidData.into())
}
fn atomic_json(root: &Path, path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    let temporary = root.join(format!(".tmp-{}", uuid::Uuid::now_v7()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        serde_json::to_writer(&mut file, value).map_err(std::io::Error::other)?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(root)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
fn private_directory(root: &Path) -> std::io::Result<()> {
    if !root.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(root)?;
        }
        #[cfg(not(unix))]
        return Err(std::io::ErrorKind::Unsupported.into());
    }
    let metadata = fs::symlink_metadata(root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !metadata.is_dir()
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
    }
    Ok(())
}

pub fn credential_free_preflight() -> Result<(), &'static str> {
    for (key, _) in std::env::vars_os() {
        if !allowed_environment_key(&key.to_string_lossy()) {
            return Err("sandbox_environment_not_credential_free");
        }
    }
    credential_paths_clean(
        [
            "/run/secrets",
            "/var/run/secrets",
            "/run/credentials",
            "/root/.aws/credentials",
            "/root/.config/gcloud/application_default_credentials.json",
            "/root/.docker/config.json",
            "/app/.env",
            "/workspace/.env",
        ]
        .iter()
        .map(Path::new),
    )?;
    Ok(())
}
fn credential_paths_clean<'a>(
    paths: impl IntoIterator<Item = &'a Path>,
) -> Result<(), &'static str> {
    for path in paths {
        if path.is_file()
            || path.is_dir()
                && fs::read_dir(path).map_or(true, |mut entries| entries.next().is_some())
        {
            return Err("sandbox_credential_mount_present");
        }
    }
    Ok(())
}

fn allowed_environment_key(key: &str) -> bool {
    matches!(
        key,
        "PATH"
            | "LANG"
            | "LC_ALL"
            | "LC_CTYPE"
            | "TZ"
            | "HOME"
            | "HOSTNAME"
            | "TERM"
            | "RUST_LOG"
            | "RUST_BACKTRACE"
            | "PYTHONNOUSERSITE"
            | "PYTHONDONTWRITEBYTECODE"
            | "PYTHONUNBUFFERED"
            | "PYTHON_VERSION"
            | "PYTHON_SHA256"
            | "GPG_KEY"
    )
}

/// Bind a private Unix endpoint; clean only a same-owner dead socket at the selected path, never bind TCP.
#[cfg(unix)]
pub async fn serve(socket: PathBuf, service: SandboxService) -> Result<(), &'static str> {
    use std::os::unix::fs::PermissionsExt;
    let parent = socket
        .parent()
        .filter(|_| socket.is_absolute())
        .ok_or("sandbox_socket_invalid")?;
    private_directory(parent).map_err(|_| "sandbox_socket_directory_invalid")?;
    if let Ok(metadata) = fs::symlink_metadata(&socket) {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        if !metadata.file_type().is_socket()
            || metadata.uid() != unsafe { libc::geteuid() }
            || std::os::unix::net::UnixStream::connect(&socket).is_ok()
        {
            return Err("sandbox_socket_already_owned");
        }
        fs::remove_file(&socket).map_err(|_| "sandbox_stale_socket_cleanup_failed")?;
    }
    let listener =
        tokio::net::UnixListener::bind(&socket).map_err(|_| "sandbox_socket_bind_failed")?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
        .map_err(|_| "sandbox_socket_permissions_failed")?;
    axum::serve(listener, service.router())
        .await
        .map_err(|_| "sandbox_server_failed")
}

#[cfg(test)]
#[path = "sandbox_service_tests.rs"]
mod tests;

#[cfg(all(test, target_os = "linux"))]
#[path = "sandbox_domain_acceptance_tests.rs"]
mod domain_acceptance_tests;
