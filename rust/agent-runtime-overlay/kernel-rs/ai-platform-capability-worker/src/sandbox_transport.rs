//! Credential-free Unix-socket boundary. No platform lease or shared service
//! bearer header crosses this transport; only purpose-signed sealed jobs do.
use crate::python_code_execution::{
    CodeExecutionError, PythonCodeExecutionRequest, PythonCodeExecutionResult, validate_request,
};
use ai_platform_capability_contract::sandbox_job::{
    SANDBOX_JOB_SCHEMA, SandboxAccess, SandboxJob, SandboxJobSigner, SandboxMessage,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

pub const MAX_SANDBOX_WIRE_BYTES: usize = 40 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "status",
    content = "outcome",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum SandboxReplyState {
    Running,
    Completed(Result<PythonCodeExecutionResult, CodeExecutionError>),
    SideEffectUnknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxReply {
    pub job_id: String,
    pub job_digest: String,
    pub state: SandboxReplyState,
}

pub struct RemoteSandboxBroker {
    socket: PathBuf,
    client: reqwest::Client,
    signer: Arc<SandboxJobSigner>,
}
impl RemoteSandboxBroker {
    pub fn new(socket: PathBuf, signer: Arc<SandboxJobSigner>) -> Result<Self, CodeExecutionError> {
        if !socket.is_absolute() {
            return Err(CodeExecutionError::Configuration);
        }
        #[cfg(unix)]
        {
            let client = reqwest::Client::builder()
                .retry(reqwest::retry::never())
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(5))
                .unix_socket(socket.as_path())
                .build()
                .map_err(|_| CodeExecutionError::Configuration)?;
            Ok(Self {
                socket,
                client,
                signer,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = signer;
            Err(CodeExecutionError::IsolationUnavailable)
        }
    }
    pub fn configured(&self) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            std::fs::symlink_metadata(&self.socket)
                .is_ok_and(|metadata| metadata.file_type().is_socket())
        }
        #[cfg(not(unix))]
        false
    }
    pub async fn ready(&self) -> bool {
        self.configured()
            && self
                .client
                .get("http://sandbox.invalid/health/ready")
                .timeout(Duration::from_secs(1))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
    }
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    async fn exchange(
        &self,
        message: SandboxMessage,
        job: &SandboxJob,
        digest: &str,
    ) -> Result<SandboxReply, CodeExecutionError> {
        let sealed = self
            .signer
            .seal(message, crate::now_epoch_ms())
            .map_err(|_| CodeExecutionError::Configuration)?;
        let body = serde_json::to_vec(&sealed).map_err(|_| CodeExecutionError::Configuration)?;
        if body.len() > MAX_SANDBOX_WIRE_BYTES {
            return Err(CodeExecutionError::InputsTooLarge);
        }
        // Fixed synthetic host; unix_socket prevents DNS/TCP/proxy use.
        let mut response = self
            .client
            .post("http://sandbox.invalid/internal/v1/jobs")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| CodeExecutionError::SideEffectUnknown)?;
        if !response.status().is_success() {
            return Err(
                if response.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
                    CodeExecutionError::IsolationUnavailable
                } else {
                    CodeExecutionError::SideEffectUnknown
                },
            );
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| CodeExecutionError::SideEffectUnknown)?
        {
            if body.len().saturating_add(chunk.len()) > MAX_SANDBOX_WIRE_BYTES {
                return Err(CodeExecutionError::SideEffectUnknown);
            }
            body.extend_from_slice(&chunk);
        }
        let reply: SandboxReply =
            serde_json::from_slice(&body).map_err(|_| CodeExecutionError::SideEffectUnknown)?;
        if reply.job_id != job.job_id || reply.job_digest != digest {
            return Err(CodeExecutionError::SideEffectUnknown);
        }
        Ok(reply)
    }
    pub async fn execute_cancellable(
        &self,
        request: PythonCodeExecutionRequest,
        cancelled: Arc<AtomicBool>,
    ) -> Result<PythonCodeExecutionResult, CodeExecutionError> {
        validate_request(&request)?;
        if cancelled.load(Ordering::Acquire) {
            return Err(CodeExecutionError::Cancelled);
        }
        if !self.configured() {
            return Err(CodeExecutionError::IsolationUnavailable);
        }
        let now = crate::now_epoch_ms();
        let job = job_from_request(&request, now)?;
        let digest = job
            .digest()
            .map_err(|_| CodeExecutionError::Configuration)?;
        let started = Instant::now();
        let deadline = Duration::from_secs(u64::from(request.limits.timeout_seconds) + 15);
        // Never automatically resubmit after an uncertain transport failure.
        let mut reply = self
            .exchange(SandboxMessage::Execute(job.clone()), &job, &digest)
            .await?;
        let mut cancel_sent = false;
        loop {
            match reply.state {
                SandboxReplyState::Completed(result) => {
                    let result = result?;
                    if result.execution_id != job.job_id {
                        return Err(CodeExecutionError::SideEffectUnknown);
                    }
                    return Ok(result);
                }
                SandboxReplyState::SideEffectUnknown => {
                    return Err(CodeExecutionError::SideEffectUnknown);
                }
                SandboxReplyState::Running => {}
            }
            if started.elapsed() >= deadline {
                return Err(CodeExecutionError::SideEffectUnknown);
            }
            let now = crate::now_epoch_ms();
            let access = SandboxAccess {
                job_id: job.job_id.clone(),
                scope: job.scope.clone(),
                job_digest: digest.clone(),
                issued_at_epoch_ms: now,
                expires_at_epoch_ms: now + 30_000,
            };
            let message = if cancelled.load(Ordering::Acquire) && !cancel_sent {
                cancel_sent = true;
                SandboxMessage::Cancel(access)
            } else {
                SandboxMessage::Poll(access)
            };
            tokio::time::sleep(Duration::from_millis(50)).await;
            reply = self.exchange(message, &job, &digest).await?;
        }
    }
}

pub(crate) fn job_from_request(
    request: &PythonCodeExecutionRequest,
    now: u64,
) -> Result<SandboxJob, CodeExecutionError> {
    let job_id = &request.lease.nonce;
    if uuid::Uuid::parse_str(job_id)
        .ok()
        .is_none_or(|id| id.to_string() != *job_id)
    {
        return Err(CodeExecutionError::Configuration);
    }
    Ok(SandboxJob {
        schema_version: SANDBOX_JOB_SCHEMA.into(),
        job_id: job_id.clone(),
        scope: request.lease.scope(),
        run_id: request.lease.run_id.clone(),
        tool_call_id: request.lease.tool_call_id.clone(),
        arguments_hash: request.arguments_hash.clone(),
        issued_at_epoch_ms: now,
        expires_at_epoch_ms: now
            .saturating_add(u64::from(request.limits.timeout_seconds) * 1000 + 30_000),
        code: request.code.clone(),
        inputs: serde_json::to_value(&request.inputs)
            .map_err(|_| CodeExecutionError::Configuration)?,
        limits: serde_json::to_value(&request.limits)
            .map_err(|_| CodeExecutionError::Configuration)?,
    })
}
