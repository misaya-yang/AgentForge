//! Single-attempt Gateway broker for Python sandbox output artifacts.
//!
//! The sandbox returns bounded output bytes to the worker, but only the
//! Gateway may persist user-visible artifacts.  Upload attempts are never
//! retried: a timeout, transport failure, 5xx, or malformed receipt leaves the
//! durable capability execution in `side_effect_unknown` rather than risking a
//! duplicate object write.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ai_platform_capability_contract::canonical_json_hash;
use async_trait::async_trait;
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use hmac::{Hmac, Mac};
use reqwest::{Client, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::python_code_execution::{CodeOutputArtifact, PythonCodeExecutionResult};

const PATH: &str = "/internal/v2/agent-capabilities/python/artifacts";
const PROOF_SCHEMA: &str = "ai-platform-capability-proof/v1";
const PROOF_TTL_SECONDS: u64 = 30;
const MIN_SECRET_BYTES: usize = 32;
const MAX_CONTENT_BYTES: usize = 24 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_SCOPE_FIELD: usize = 255;
const MAX_TOOL_CALL_ID: usize = 160;
const MAX_FILENAME_BYTES: usize = 255;
const MAX_OUTPUT_FILES: usize = 64;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug)]
pub struct PythonArtifactBrokerConfig {
    pub gateway_url: String,
    pub internal_token: String,
    pub proof_secret: String,
}

impl PythonArtifactBrokerConfig {
    fn endpoint(&self) -> Result<Url, PythonArtifactBrokerError> {
        let mut url =
            Url::parse(&self.gateway_url).map_err(|_| PythonArtifactBrokerError::Configuration)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || self.internal_token.len() < MIN_SECRET_BYTES
            || self.proof_secret.len() < MIN_SECRET_BYTES
        {
            return Err(PythonArtifactBrokerError::Configuration);
        }
        url.set_path(PATH);
        Ok(url)
    }
}

/// Durable scope copied from the capability execution record, never from the
/// sandbox lease.  In particular, `execution_id` is the durable capability
/// execution id that Gateway idempotency and proof verification bind to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PythonArtifactUploadContext {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub execution_id: String,
    pub run_id: String,
    pub tool_call_id: String,
    pub arguments_hash: String,
}

impl PythonArtifactUploadContext {
    fn validate(&self) -> Result<(), PythonArtifactBrokerError> {
        for field in [
            &self.tenant_id,
            &self.user_id,
            &self.session_id,
            &self.execution_id,
            &self.run_id,
        ] {
            if !valid_text(field, MAX_SCOPE_FIELD) {
                return Err(PythonArtifactBrokerError::InvalidMetadata);
            }
        }
        if !valid_text(&self.tool_call_id, MAX_TOOL_CALL_ID)
            || !valid_sha256_prefixed(&self.arguments_hash)
            || Uuid::parse_str(&self.execution_id).is_err()
            || Uuid::parse_str(&self.run_id).is_err()
        {
            return Err(PythonArtifactBrokerError::InvalidMetadata);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonArtifactRecord {
    pub artifact_id: String,
    pub download_url: String,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum PythonArtifactBrokerError {
    #[error("python artifact broker configuration is invalid")]
    Configuration,
    #[error("python artifact metadata is invalid")]
    InvalidMetadata,
    #[error("python artifact upload was rejected")]
    Failed,
    #[error("python artifact upload outcome is unknown")]
    SideEffectUnknown,
}

#[async_trait]
pub trait PythonArtifactStore: Send + Sync {
    async fn put_once(
        &self,
        context: &PythonArtifactUploadContext,
        artifact: &CodeOutputArtifact,
    ) -> Result<PythonArtifactRecord, PythonArtifactBrokerError>;
}

#[derive(Clone)]
pub struct ReqwestPythonArtifactStore {
    client: Client,
    config: PythonArtifactBrokerConfig,
}

impl ReqwestPythonArtifactStore {
    pub fn new(
        client: Client,
        config: PythonArtifactBrokerConfig,
    ) -> Result<Self, PythonArtifactBrokerError> {
        config.endpoint()?;
        Ok(Self { client, config })
    }

    fn body(
        context: &PythonArtifactUploadContext,
        artifact: &CodeOutputArtifact,
    ) -> Result<Value, PythonArtifactBrokerError> {
        context.validate()?;
        validate_artifact(artifact)?;
        // This set deliberately matches the Gateway request model exactly.
        Ok(json!({
            "tool_call_id": context.tool_call_id,
            "arguments_hash": context.arguments_hash,
            "filename": artifact.filename,
            "mime_type": artifact.mime_type,
            "sha256": artifact.sha256,
            "size_bytes": artifact.size_bytes,
            "content_base64": artifact.content_base64,
        }))
    }

    fn proof(
        &self,
        context: &PythonArtifactUploadContext,
        body: &Value,
    ) -> Result<String, PythonArtifactBrokerError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| PythonArtifactBrokerError::Configuration)?
            .as_secs();
        let expires_at = now
            .checked_add(PROOF_TTL_SECONDS)
            .ok_or(PythonArtifactBrokerError::Configuration)?;
        let body_hash = canonical_json_hash(body)
            .map_err(|_| PythonArtifactBrokerError::InvalidMetadata)?
            .strip_prefix("sha256:")
            .ok_or(PythonArtifactBrokerError::InvalidMetadata)?
            .to_owned();
        // Keep the proof representation byte-for-byte compatible with the
        // Office artifact broker and Gateway proof verifier.
        let mut unsigned = BTreeMap::<String, Value>::new();
        for (key, value) in [
            ("body_sha256", Value::String(body_hash)),
            ("execution_id", Value::String(context.execution_id.clone())),
            ("expires_at", json!(expires_at)),
            ("method", Value::String("POST".into())),
            ("nonce", Value::String(Uuid::now_v7().simple().to_string())),
            ("path", Value::String(PATH.into())),
            ("run_id", Value::String(context.run_id.clone())),
            ("schema_version", Value::String(PROOF_SCHEMA.into())),
            ("session_id", Value::String(context.session_id.clone())),
            ("tenant_id", Value::String(context.tenant_id.clone())),
            ("user_id", Value::String(context.user_id.clone())),
        ] {
            unsigned.insert(key.into(), value);
        }
        let bytes =
            serde_json::to_vec(&unsigned).map_err(|_| PythonArtifactBrokerError::Configuration)?;
        let mut mac = HmacSha256::new_from_slice(self.config.proof_secret.as_bytes())
            .map_err(|_| PythonArtifactBrokerError::Configuration)?;
        mac.update(&bytes);
        unsigned.insert(
            "signature".into(),
            Value::String(hex::encode(mac.finalize().into_bytes())),
        );
        Ok(format!(
            "v1.{}",
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&unsigned)
                    .map_err(|_| PythonArtifactBrokerError::Configuration)?
            )
        ))
    }
}

#[async_trait]
impl PythonArtifactStore for ReqwestPythonArtifactStore {
    async fn put_once(
        &self,
        context: &PythonArtifactUploadContext,
        artifact: &CodeOutputArtifact,
    ) -> Result<PythonArtifactRecord, PythonArtifactBrokerError> {
        let body = Self::body(context, artifact)?;
        let proof = self.proof(context, &body)?;
        let request = self
            .client
            .post(self.config.endpoint()?)
            .header("x-ai-platform-internal-token", &self.config.internal_token)
            .header("x-ai-tenant-id", &context.tenant_id)
            .header("x-ai-user-id", &context.user_id)
            .header("x-ai-session-id", &context.session_id)
            .header("x-ai-tool-call-id", &context.tool_call_id)
            .header("x-ai-arguments-hash", &context.arguments_hash)
            .header("x-ai-capability-proof", proof);
        let response = crate::trace_context::apply(
            request,
            &context.run_id,
            &context.run_id,
            &context.execution_id,
        )
        .json(&body)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|_| PythonArtifactBrokerError::SideEffectUnknown)?;
        let status = response.status();
        if !status.is_success() {
            return Err(status_error(status));
        }
        let bytes = bounded_response(response).await?;
        let record: PythonArtifactRecord = serde_json::from_slice(&bytes)
            .map_err(|_| PythonArtifactBrokerError::SideEffectUnknown)?;
        if !valid_record(&record, artifact) {
            return Err(PythonArtifactBrokerError::SideEffectUnknown);
        }
        Ok(record)
    }
}

pub async fn persist_output_artifacts(
    store: &dyn PythonArtifactStore,
    context: &PythonArtifactUploadContext,
    output_files: &[CodeOutputArtifact],
) -> Result<Vec<PythonArtifactRecord>, PythonArtifactBrokerError> {
    if output_files.len() > MAX_OUTPUT_FILES {
        return Err(PythonArtifactBrokerError::InvalidMetadata);
    }
    let mut total = 0_usize;
    let mut records = Vec::with_capacity(output_files.len());
    for artifact in output_files {
        let next_total = total.checked_add(artifact.size_bytes);
        let Some(next_total) = next_total.filter(|total| *total <= MAX_CONTENT_BYTES) else {
            return Err(if records.is_empty() {
                PythonArtifactBrokerError::InvalidMetadata
            } else {
                PythonArtifactBrokerError::SideEffectUnknown
            });
        };
        total = next_total;
        // Sequential single attempts preserve deterministic output order.  If
        // a previous artifact was accepted, even a later deterministic 4xx
        // means the overall execution has a partial external side effect and
        // must reconcile as unknown rather than terminally failed.
        match store.put_once(context, artifact).await {
            Ok(record) => records.push(record),
            Err(_) if !records.is_empty() => {
                return Err(PythonArtifactBrokerError::SideEffectUnknown);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(records)
}

/// Build the terminal code-execution result after Gateway persistence.  The
/// sandbox's ephemeral lease id is replaced with the durable execution id and
/// raw base64 files are deliberately omitted from the event payload.
pub async fn terminal_result(
    store: &dyn PythonArtifactStore,
    context: &PythonArtifactUploadContext,
    mut result: PythonCodeExecutionResult,
) -> Result<Value, PythonArtifactBrokerError> {
    if result.side_effect_state != "known" {
        return Err(PythonArtifactBrokerError::SideEffectUnknown);
    }
    // Only a successful sandbox outcome may publish generated files.  Failed
    // code still returns its bounded streams and exit status, but no partial
    // filesystem output becomes a user-visible artifact.
    let records = if result.status == "succeeded" {
        persist_output_artifacts(store, context, &result.output_files).await?
    } else {
        Vec::new()
    };
    result.execution_id = context.execution_id.clone();
    result.output_files.clear();
    let mut value =
        serde_json::to_value(result).map_err(|_| PythonArtifactBrokerError::InvalidMetadata)?;
    let object = value
        .as_object_mut()
        .ok_or(PythonArtifactBrokerError::InvalidMetadata)?;
    // `output_files` was the sandbox-only transfer representation.  Terminal
    // events contain compact Gateway metadata only, preventing base64 output
    // from crossing the 128 KiB event payload boundary.
    object.remove("output_files");
    object.insert(
        "artifacts".into(),
        serde_json::to_value(records).map_err(|_| PythonArtifactBrokerError::InvalidMetadata)?,
    );
    Ok(value)
}

async fn bounded_response(mut response: Response) -> Result<Vec<u8>, PythonArtifactBrokerError> {
    if response
        .content_length()
        .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
    {
        return Err(PythonArtifactBrokerError::SideEffectUnknown);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| PythonArtifactBrokerError::SideEffectUnknown)?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(PythonArtifactBrokerError::SideEffectUnknown);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn validate_artifact(artifact: &CodeOutputArtifact) -> Result<(), PythonArtifactBrokerError> {
    if !valid_text(&artifact.filename, MAX_FILENAME_BYTES)
        || artifact.filename == "."
        || artifact.filename == ".."
        || artifact.filename.contains('/')
        || artifact.filename.contains('\\')
        || !valid_sha256(&artifact.sha256)
        || artifact.size_bytes > MAX_CONTENT_BYTES
        || artifact
            .mime_type
            .as_deref()
            .is_some_and(|mime| !valid_mime(mime))
    {
        return Err(PythonArtifactBrokerError::InvalidMetadata);
    }
    let content = STANDARD
        .decode(&artifact.content_base64)
        .map_err(|_| PythonArtifactBrokerError::InvalidMetadata)?;
    if content.len() != artifact.size_bytes
        || content.len() > MAX_CONTENT_BYTES
        || hex::encode(Sha256::digest(&content)) != artifact.sha256
    {
        return Err(PythonArtifactBrokerError::InvalidMetadata);
    }
    Ok(())
}

fn valid_record(record: &PythonArtifactRecord, source: &CodeOutputArtifact) -> bool {
    record.artifact_id.len() == 20
        && record.artifact_id.starts_with("art_")
        && record.artifact_id[4..]
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        && record
            .download_url
            .starts_with("/api/v1/assistant/artifacts/")
        && valid_text(&record.download_url, MAX_SCOPE_FIELD)
        && record.filename == source.filename
        && valid_mime(&record.mime_type)
        && record.size_bytes == source.size_bytes as u64
        && record.sha256 == source.sha256
}

fn status_error(status: StatusCode) -> PythonArtifactBrokerError {
    if status.is_client_error() {
        PythonArtifactBrokerError::Failed
    } else {
        PythonArtifactBrokerError::SideEffectUnknown
    }
}

fn valid_text(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.bytes().any(|byte| byte.is_ascii_control())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn valid_sha256_prefixed(value: &str) -> bool {
    value.len() == 71 && value.starts_with("sha256:") && valid_sha256(&value[7..])
}

fn valid_mime(value: &str) -> bool {
    let mut parts = value.split('/');
    let Some(kind) = parts.next() else {
        return false;
    };
    let Some(subtype) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && !kind.is_empty()
        && !subtype.is_empty()
        && kind.bytes().chain(subtype.bytes()).all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                )
        })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn context() -> PythonArtifactUploadContext {
        PythonArtifactUploadContext {
            tenant_id: "tenant".into(),
            user_id: "user".into(),
            session_id: "session".into(),
            execution_id: "00000000-0000-4000-8000-000000000001".into(),
            run_id: "00000000-0000-4000-8000-000000000002".into(),
            tool_call_id: "call".into(),
            arguments_hash: format!("sha256:{}", "a".repeat(64)),
        }
    }

    fn artifact() -> CodeOutputArtifact {
        let content = b"chart";
        CodeOutputArtifact {
            filename: "chart.txt".into(),
            mime_type: Some("text/plain".into()),
            content_base64: STANDARD.encode(content),
            size_bytes: content.len(),
            sha256: hex::encode(Sha256::digest(content)),
        }
    }

    #[test]
    fn body_is_the_strict_gateway_contract_without_scope_or_raw_extras() {
        let body = ReqwestPythonArtifactStore::body(&context(), &artifact()).unwrap();
        let object = body.as_object().unwrap();
        assert_eq!(object.len(), 7);
        for field in [
            "tool_call_id",
            "arguments_hash",
            "filename",
            "mime_type",
            "sha256",
            "size_bytes",
            "content_base64",
        ] {
            assert!(object.contains_key(field));
        }
        assert!(!object.contains_key("execution_id"));
        assert!(!object.contains_key("tenant_id"));
    }

    #[test]
    fn proof_binds_the_durable_execution_id_not_a_lease_id() {
        let broker = ReqwestPythonArtifactStore::new(
            Client::new(),
            PythonArtifactBrokerConfig {
                gateway_url: "http://gateway:8080/base".into(),
                internal_token: "x".repeat(32),
                proof_secret: "y".repeat(32),
            },
        )
        .unwrap();
        let body = ReqwestPythonArtifactStore::body(&context(), &artifact()).unwrap();
        let proof = broker.proof(&context(), &body).unwrap();
        let encoded = proof.strip_prefix("v1.").unwrap();
        let value: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded).unwrap()).unwrap();
        assert_eq!(
            value["execution_id"],
            "00000000-0000-4000-8000-000000000001"
        );
        assert_eq!(broker.config.endpoint().unwrap().path(), PATH);
    }

    #[test]
    fn only_a_first_deterministic_client_rejection_is_failed() {
        assert_eq!(
            status_error(StatusCode::UNPROCESSABLE_ENTITY),
            PythonArtifactBrokerError::Failed
        );
        assert_eq!(
            status_error(StatusCode::SERVICE_UNAVAILABLE),
            PythonArtifactBrokerError::SideEffectUnknown
        );
    }

    #[derive(Default)]
    struct MemoryStore(Mutex<Vec<String>>);

    #[async_trait]
    impl PythonArtifactStore for MemoryStore {
        async fn put_once(
            &self,
            context: &PythonArtifactUploadContext,
            artifact: &CodeOutputArtifact,
        ) -> Result<PythonArtifactRecord, PythonArtifactBrokerError> {
            self.0.lock().unwrap().push(context.execution_id.clone());
            Ok(PythonArtifactRecord {
                artifact_id: "art_output".into(),
                download_url: "/api/v1/assistant/artifacts/art_output/download".into(),
                filename: artifact.filename.clone(),
                mime_type: artifact
                    .mime_type
                    .clone()
                    .unwrap_or_else(|| "application/octet-stream".into()),
                size_bytes: artifact.size_bytes as u64,
                sha256: artifact.sha256.clone(),
            })
        }
    }

    #[tokio::test]
    async fn persistence_keeps_order_and_uses_durable_execution_scope() {
        let store = MemoryStore::default();
        let records = persist_output_artifacts(&store, &context(), &[artifact()])
            .await
            .unwrap();
        assert_eq!(records[0].filename, "chart.txt");
        assert_eq!(
            store.0.lock().unwrap().as_slice(),
            ["00000000-0000-4000-8000-000000000001"]
        );
    }

    #[tokio::test]
    async fn terminal_payload_replaces_raw_output_files_with_compact_artifacts() {
        let store = MemoryStore::default();
        let value = terminal_result(
            &store,
            &context(),
            PythonCodeExecutionResult {
                execution_id: "lease-id-must-not-escape".into(),
                status: "succeeded".into(),
                stdout: "ok".into(),
                stderr: String::new(),
                output_files: vec![artifact()],
                duration_ms: 1,
                exit_code: Some(0),
                error_message: None,
                side_effect_state: "known".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            value["execution_id"].as_str(),
            Some("00000000-0000-4000-8000-000000000001")
        );
        assert!(value.get("output_files").is_none());
        assert_eq!(
            value["artifacts"][0]["artifact_id"].as_str(),
            Some("art_output")
        );
        assert_eq!(value["stdout"].as_str(), Some("ok"));
        assert_eq!(value["exit_code"].as_i64(), Some(0));
    }

    struct PartiallyFailingStore;

    #[async_trait]
    impl PythonArtifactStore for PartiallyFailingStore {
        async fn put_once(
            &self,
            _context: &PythonArtifactUploadContext,
            artifact: &CodeOutputArtifact,
        ) -> Result<PythonArtifactRecord, PythonArtifactBrokerError> {
            if artifact.filename == "rejected.txt" {
                return Err(PythonArtifactBrokerError::Failed);
            }
            Ok(PythonArtifactRecord {
                artifact_id: "art_output".into(),
                download_url: "/api/v1/assistant/artifacts/art_output/download".into(),
                filename: artifact.filename.clone(),
                mime_type: artifact
                    .mime_type
                    .clone()
                    .unwrap_or_else(|| "application/octet-stream".into()),
                size_bytes: artifact.size_bytes as u64,
                sha256: artifact.sha256.clone(),
            })
        }
    }

    #[tokio::test]
    async fn later_rejection_after_a_persisted_artifact_is_side_effect_unknown() {
        let mut rejected = artifact();
        rejected.filename = "rejected.txt".into();
        let error =
            persist_output_artifacts(&PartiallyFailingStore, &context(), &[artifact(), rejected])
                .await
                .unwrap_err();
        assert_eq!(error, PythonArtifactBrokerError::SideEffectUnknown);
    }
}
