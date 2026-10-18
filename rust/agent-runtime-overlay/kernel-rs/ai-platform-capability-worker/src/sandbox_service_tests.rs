//! Protocol/journal regressions use a deterministic executor. These tests do
//! not replace the ignored real Linux namespace/escape acceptance suite.
use super::*;
use crate::python_code_execution::{CodeInputAttachment, PythonSandboxLimits};
use ai_platform_capability_contract::sandbox_job::{SANDBOX_JOB_SCHEMA, SandboxJobSigner};
use std::sync::atomic::AtomicUsize;

struct TempRoot(PathBuf);
impl TempRoot {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("agentforge-sandbox-test-{}", uuid::Uuid::now_v7())))
    }
}
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct FakeExecutor {
    calls: AtomicUsize,
    hold: AtomicBool,
}
impl FakeExecutor {
    fn new(hold: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            hold: AtomicBool::new(hold),
        })
    }
}
#[async_trait::async_trait]
impl SandboxExecutor for FakeExecutor {
    fn available(&self) -> bool {
        true
    }
    async fn execute(
        &self,
        request: PythonCodeExecutionRequest,
        cancel: Arc<AtomicBool>,
    ) -> Result<PythonCodeExecutionResult, CodeExecutionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        while self.hold.load(Ordering::Acquire) {
            if cancel.load(Ordering::Acquire) {
                return Err(CodeExecutionError::Cancelled);
            }
            tokio::task::yield_now().await;
        }
        Ok(PythonCodeExecutionResult {
            execution_id: request.lease.lease_id,
            status: "succeeded".into(),
            stdout: "fake-result".into(),
            stderr: String::new(),
            output_files: vec![],
            duration_ms: 1,
            exit_code: Some(0),
            error_message: None,
            side_effect_state: "known".into(),
        })
    }
}
fn keys() -> (SandboxJobSigner, SandboxJobVerifier) {
    let signer = SandboxJobSigner::new("control".into(), [9; 32]).unwrap();
    let verifier = SandboxJobVerifier::new(BTreeMap::from([(
        "control".into(),
        signer.public_key_bytes(),
    )]))
    .unwrap();
    (signer, verifier)
}
fn job() -> SandboxJob {
    let now = crate::now_epoch_ms();
    SandboxJob {
        schema_version: SANDBOX_JOB_SCHEMA.into(),
        job_id: uuid::Uuid::now_v7().to_string(),
        scope: CapabilityScopeV2 {
            tenant_id: "tenant-a".into(),
            user_id: "user-a".into(),
            session_id: "session-a".into(),
        },
        run_id: uuid::Uuid::now_v7().to_string(),
        tool_call_id: "call-a".into(),
        arguments_hash: format!("sha256:{}", "a".repeat(64)),
        issued_at_epoch_ms: now,
        expires_at_epoch_ms: now + 60_000,
        code: "print('test')".into(),
        inputs: serde_json::json!([]),
        limits: serde_json::to_value(PythonSandboxLimits::default()).unwrap(),
    }
}
fn service(root: &Path, executor: Arc<FakeExecutor>) -> SandboxService {
    let (_, verifier) = keys();
    SandboxService::from_parts(
        SandboxServiceConfig {
            journal_root: root.into(),
            concurrency: 1,
            max_records: 4,
            ..Default::default()
        },
        verifier,
        executor,
    )
    .unwrap()
}
fn access(job: &SandboxJob) -> SandboxAccess {
    let now = crate::now_epoch_ms();
    SandboxAccess {
        job_id: job.job_id.clone(),
        scope: job.scope.clone(),
        job_digest: job.digest().unwrap(),
        issued_at_epoch_ms: now,
        expires_at_epoch_ms: now + 30_000,
    }
}
async fn completed(service: &SandboxService, job: &SandboxJob) -> SandboxReplyState {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let reply = service
                .access(access(job), false)
                .await
                .unwrap_or_else(|_| panic!("poll failed"));
            if !matches!(reply.state, SandboxReplyState::Running) {
                return reply.state;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("executor did not finish"))
}

#[tokio::test]
async fn saturated_duplicate_replays_and_concurrent_key_dispatches_once() {
    let root = TempRoot::new();
    let executor = FakeExecutor::new(true);
    let service = service(&root.0, executor.clone());
    let job = job();
    let (left, right) = tokio::join!(service.submit(job.clone()), service.submit(job.clone()));
    assert!(left.is_ok() && right.is_ok());
    let mut fresh = job.clone();
    fresh.job_id = uuid::Uuid::now_v7().to_string();
    assert!(matches!(
        service.submit(fresh.clone()).await,
        Err((StatusCode::SERVICE_UNAVAILABLE, _))
    ));
    assert!(!meta_path(&root.0, &fresh.job_id).exists());
    executor.hold.store(false, Ordering::Release);
    assert!(matches!(
        completed(&service, &job).await,
        SandboxReplyState::Completed(Ok(_))
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        service
            .submit(job.clone())
            .await
            .unwrap_or_else(|_| panic!("replay failed"))
            .state,
        SandboxReplyState::Completed(Ok(_))
    ));
}

#[tokio::test]
async fn body_conflict_and_cross_tenant_or_actor_access_are_closed() {
    let root = TempRoot::new();
    let executor = FakeExecutor::new(true);
    let service = service(&root.0, executor.clone());
    let job = job();
    assert!(service.submit(job.clone()).await.is_ok());
    let mut changed = job.clone();
    changed.code = "print('different')".into();
    assert!(matches!(
        service.submit(changed).await,
        Err((StatusCode::CONFLICT, _))
    ));
    for field in ["tenant", "user", "session"] {
        let mut access = access(&job);
        match field {
            "tenant" => access.scope.tenant_id = "other".into(),
            "user" => access.scope.user_id = "other".into(),
            _ => access.scope.session_id = "other".into(),
        }
        assert!(matches!(
            service.access(access, true).await,
            Err((StatusCode::NOT_FOUND, _))
        ));
    }
    executor.hold.store(false, Ordering::Release);
    let _ = completed(&service, &job).await;
}

#[tokio::test]
async fn signed_cancel_reaches_execution_and_is_retained() {
    let root = TempRoot::new();
    let executor = FakeExecutor::new(true);
    let service = service(&root.0, executor);
    let job = job();
    let (signer, _) = keys();
    let execute = signer
        .seal(SandboxMessage::Execute(job.clone()), crate::now_epoch_ms())
        .unwrap();
    assert!(handle(State(service.clone()), Json(execute)).await.is_ok());
    let cancel = signer
        .seal(SandboxMessage::Cancel(access(&job)), crate::now_epoch_ms())
        .unwrap();
    assert!(handle(State(service.clone()), Json(cancel)).await.is_ok());
    assert!(matches!(
        completed(&service, &job).await,
        SandboxReplyState::Completed(Err(CodeExecutionError::Cancelled))
    ));
}

#[tokio::test]
async fn bad_signature_never_creates_a_record() {
    let root = TempRoot::new();
    let executor = FakeExecutor::new(false);
    let service = service(&root.0, executor.clone());
    let job = job();
    let (signer, _) = keys();
    let mut execute = signer
        .seal(SandboxMessage::Execute(job.clone()), crate::now_epoch_ms())
        .unwrap();
    execute.signature_hex = "00".repeat(64);
    assert!(matches!(
        handle(State(service), Json(execute)).await,
        Err((StatusCode::FORBIDDEN, _))
    ));
    assert!(!meta_path(&root.0, &job.job_id).exists());
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn restart_never_reexecutes_unfinished_or_completed_jobs() {
    let root = TempRoot::new();
    private_directory(&root.0).unwrap();
    let unfinished = job();
    let mut finished = job();
    finished.job_id = uuid::Uuid::now_v7().to_string();
    for job in [&unfinished, &finished] {
        atomic_json(
            &root.0,
            &meta_path(&root.0, &job.job_id),
            &JournalRecord {
                job_id: job.job_id.clone(),
                scope: job.scope.clone(),
                digest: job.digest().unwrap(),
                expires: job.expires_at_epoch_ms,
                state: JournalState::Running,
            },
        )
        .unwrap();
    }
    // Model a crash after durable result but before the Finished marker.
    let result: Result<PythonCodeExecutionResult, CodeExecutionError> =
        Err(CodeExecutionError::Cancelled);
    atomic_json(&root.0, &result_path(&root.0, &finished.job_id), &result).unwrap();
    let executor = FakeExecutor::new(false);
    let service = service(&root.0, executor.clone());
    assert!(matches!(
        service
            .submit(unfinished)
            .await
            .unwrap_or_else(|_| panic!("replay failed"))
            .state,
        SandboxReplyState::SideEffectUnknown
    ));
    assert!(matches!(
        service
            .submit(finished)
            .await
            .unwrap_or_else(|_| panic!("replay failed"))
            .state,
        SandboxReplyState::Completed(Err(CodeExecutionError::Cancelled))
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    let (_, verifier) = keys();
    assert!(
        SandboxService::from_parts(
            SandboxServiceConfig {
                journal_root: root.0.clone(),
                ..Default::default()
            },
            verifier,
            executor
        )
        .is_err(),
        "a second process owner must not share the journal"
    );
}

#[test]
fn platform_credential_environment_is_never_allowlisted() {
    for key in [
        "AI_PLATFORM_INTERNAL_TOKEN",
        "AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET",
        "POSTGRES_PASSWORD",
        "DATABASE_URL",
        "OPENAI_API_KEY",
        "AWS_ACCESS_KEY_ID",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "AI_PLATFORM_CAPABILITY_PROOF_SECRET",
    ] {
        assert!(!allowed_environment_key(key), "{key}");
    }
    for key in ["PATH", "LANG", "HOSTNAME"] {
        assert!(allowed_environment_key(key));
    }
}

#[test]
fn local_request_contains_only_synthetic_non_authority_metadata() {
    let request = local_request(&job()).unwrap();
    assert!(request.lease.signature.is_empty());
    assert!(request.lease.approval_id.is_none());
    assert_eq!(request.lease.attempt_id, "sealed-sandbox");
    assert_eq!(request.inputs, Vec::<CodeInputAttachment>::new());
}

#[cfg(unix)]
#[tokio::test]
async fn real_unix_transport_roundtrip_uses_sealed_jobs_and_no_platform_bearer() {
    use crate::sandbox_transport::RemoteSandboxBroker;
    let root = TempRoot::new();
    let executor = FakeExecutor::new(false);
    let service = service(&root.0.join("journal"), executor.clone());
    let socket_dir = root.0.join("socket");
    private_directory(&socket_dir).unwrap();
    let socket = socket_dir.join("control.sock");
    let socket_for_server = socket.clone();
    let server = tokio::spawn(async move { serve(socket_for_server, service).await });
    for _ in 0..1000 {
        if socket.exists() {
            break;
        }
        tokio::task::yield_now().await;
    }
    let (signer, _) = keys();
    let broker = RemoteSandboxBroker::new(socket, Arc::new(signer)).unwrap();
    let job = job();
    let mut request = local_request(&job).unwrap();
    // Model the control plane's real lease metadata; these strings MUST NOT
    // appear in the sealed job, synthetic sandbox request, or journal.
    request.lease.signature = "control-lease-proof-must-not-cross".into();
    request.lease.approval_id = Some("control-approval-token-must-not-cross".into());
    request.lease.nonce = job.job_id.clone();
    let sealed_job =
        crate::sandbox_transport::job_from_request(&request, crate::now_epoch_ms()).unwrap();
    let serialized = serde_json::to_string(&sealed_job).unwrap();
    assert!(!serialized.contains("must-not-cross"));
    let result = broker
        .execute_cancellable(request, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert_eq!(result.execution_id, job.job_id);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    server.abort();
    let _ = server.await;
}

#[test]
fn file_credentials_are_detected_without_reading_their_contents() {
    let root = TempRoot::new();
    private_directory(&root.0).unwrap();
    assert!(credential_paths_clean([root.0.as_path()]).is_ok());
    let secret = root.0.join("platform-token");
    fs::write(&secret, b"not-a-real-secret").unwrap();
    assert!(credential_paths_clean([root.0.as_path()]).is_err());
    assert!(credential_paths_clean([secret.as_path()]).is_err());
}

#[tokio::test]
async fn a_strong_sandbox_accepts_one_original_execution_and_never_recycles() {
    let root = TempRoot::new();
    let executor = FakeExecutor::new(false);
    let (_, verifier) = keys();
    // from_parts isolates journal/admission behavior; cryptographic attestation
    // and real tier execution have their own independent gates.
    let service = SandboxService::from_parts(
        SandboxServiceConfig {
            journal_root: root.0.clone(),
            concurrency: 1,
            max_records: 1,
            strong_tier: Some(crate::sandbox_tier::StrongTierConfig {
                runtime: crate::sandbox_tier::StrongRuntime::Gvisor,
                evidence_file: "/test-only/evidence".into(),
                public_keyring_file: "/test-only/keys".into(),
            }),
            ..Default::default()
        },
        verifier,
        executor.clone(),
    )
    .unwrap();
    let original = job();
    service.submit(original.clone()).await.unwrap();
    assert!(matches!(
        completed(&service, &original).await,
        SandboxReplyState::Completed(Ok(_))
    ));
    assert!(service.submit(original).await.is_ok());
    let different = job();
    assert!(matches!(
        service.submit(different).await,
        Err((StatusCode::SERVICE_UNAVAILABLE, _))
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
}
