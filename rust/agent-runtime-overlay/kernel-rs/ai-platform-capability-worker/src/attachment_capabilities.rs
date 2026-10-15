//! Scope-bound attachment reader.
//!
//! Attachment bytes remain in Gateway-owned artifact storage. The worker sends
//! only a short-lived, execution-bound proof and bounded read arguments; the
//! Gateway performs the scoped fetch and format parser. Provider credentials
//! and storage URLs never cross this boundary.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ai_platform_capability_contract::{RuntimeCapabilityLeaseV1, canonical_json_hash};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use reqwest::{Client, Response, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::Sha256;
use thiserror::Error;
use uuid::Uuid;

const PATH: &str = "/internal/v2/agent-capabilities/attachments/read";
const PROOF_SCHEMA: &str = "ai-platform-capability-proof/v1";
const PROOF_TTL_SECONDS: u64 = 30;
const MIN_SECRET_BYTES: usize = 32;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_CONTENT_CHARS: usize = 500_000;
const MAX_ATTACHMENT_ID_BYTES: usize = 128;
type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AttachmentCapabilityError {
    #[error("attachment capability configuration is invalid")]
    Configuration,
    #[error("attachment arguments are invalid")]
    Arguments,
    #[error("attachment argument hash mismatch")]
    ArgumentsHashMismatch,
    #[error("attachment is unavailable")]
    Unavailable,
    #[error("attachment read outcome is unknown")]
    SideEffectUnknown,
    #[error("attachment response is malformed")]
    MalformedResponse,
    #[error("attachment was rejected")]
    Rejected,
}

#[derive(Clone, Debug)]
pub struct AttachmentCapabilityConfig {
    pub gateway_url: String,
    pub internal_token: String,
    pub proof_secret: String,
}

impl AttachmentCapabilityConfig {
    fn endpoint(&self) -> Result<Url, AttachmentCapabilityError> {
        let mut url =
            Url::parse(&self.gateway_url).map_err(|_| AttachmentCapabilityError::Configuration)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || self.internal_token.len() < MIN_SECRET_BYTES
            || self.proof_secret.len() < MIN_SECRET_BYTES
        {
            return Err(AttachmentCapabilityError::Configuration);
        }
        url.set_path(PATH);
        Ok(url)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentReadArguments {
    pub attachment_id: String,
    /// `metadata` returns metadata only; `content` returns bounded text plus
    /// metadata. The Gateway chooses the parser from trusted MIME/filename.
    pub operation: String,
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,
}

fn default_max_chars() -> usize {
    100_000
}

impl AttachmentReadArguments {
    fn validate(&self) -> Result<(), AttachmentCapabilityError> {
        if self.attachment_id.is_empty()
            || self.attachment_id.len() > MAX_ATTACHMENT_ID_BYTES
            || self
                .attachment_id
                .bytes()
                .any(|byte| byte.is_ascii_control())
            || self.attachment_id.contains('/')
            || self.attachment_id.contains('\\')
            || self.attachment_id == "."
            || self.attachment_id == ".."
            || !matches!(self.operation.as_str(), "metadata" | "content")
            || self.max_chars == 0
            || self.max_chars > MAX_CONTENT_CHARS
        {
            return Err(AttachmentCapabilityError::Arguments);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AttachmentReadResult {
    pub attachment_id: String,
    pub filename: String,
    pub mime_type: String,
    pub format: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub metadata: Value,
    pub content: Option<String>,
    pub truncated: bool,
}

#[derive(Clone)]
pub struct AttachmentCapabilityBroker {
    client: Client,
    config: AttachmentCapabilityConfig,
}

impl AttachmentCapabilityBroker {
    pub fn new(
        client: Client,
        config: AttachmentCapabilityConfig,
    ) -> Result<Self, AttachmentCapabilityError> {
        config.endpoint()?;
        Ok(Self { client, config })
    }

    pub async fn read(
        &self,
        lease: RuntimeCapabilityLeaseV1,
        arguments: AttachmentReadArguments,
    ) -> Result<AttachmentReadResult, AttachmentCapabilityError> {
        // Compatibility helper for isolated callers. The runtime integration
        // must use `read_scoped` with the durable execution id.
        let execution_id = lease.lease_id.clone();
        self.read_scoped(lease, execution_id, arguments).await
    }

    pub async fn read_scoped(
        &self,
        lease: RuntimeCapabilityLeaseV1,
        execution_id: String,
        arguments: AttachmentReadArguments,
    ) -> Result<AttachmentReadResult, AttachmentCapabilityError> {
        let raw_arguments =
            serde_json::to_value(arguments).map_err(|_| AttachmentCapabilityError::Arguments)?;
        self.read_scoped_value(lease, execution_id, raw_arguments)
            .await
    }

    pub async fn read_scoped_value(
        &self,
        lease: RuntimeCapabilityLeaseV1,
        execution_id: String,
        raw_arguments: Value,
    ) -> Result<AttachmentReadResult, AttachmentCapabilityError> {
        if lease.capability_id != "read_attachment"
            || lease.effect != ai_platform_capability_contract::CapabilityEffect::Read
            || execution_id.is_empty()
            || execution_id.len() > 255
            || execution_id.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(AttachmentCapabilityError::Arguments);
        }
        let (arguments, body) = normalize_bound_arguments(raw_arguments, &lease.arguments_hash)?;
        let proof = self.proof(&lease, &execution_id, &body)?;
        let request = self
            .client
            .post(self.config.endpoint()?)
            .header("x-ai-platform-internal-token", &self.config.internal_token)
            .header("x-ai-tenant-id", &lease.tenant_id)
            .header("x-ai-user-id", &lease.user_id)
            .header("x-ai-session-id", &lease.session_id)
            .header("x-ai-tool-call-id", &lease.tool_call_id)
            .header("x-ai-capability-proof", proof);
        let response =
            crate::trace_context::apply(request, &lease.run_id, &lease.run_id, &execution_id)
                .json(&body)
                .timeout(Duration::from_secs(PROOF_TTL_SECONDS + 10))
                .send()
                .await
                .map_err(|_| AttachmentCapabilityError::SideEffectUnknown)?;
        if response.status().is_server_error() {
            return Err(AttachmentCapabilityError::SideEffectUnknown);
        }
        if !response.status().is_success() {
            return Err(AttachmentCapabilityError::Rejected);
        }
        let bytes = bounded_response(response).await?;
        let result: AttachmentReadResult = serde_json::from_slice(&bytes)
            .map_err(|_| AttachmentCapabilityError::MalformedResponse)?;
        validate_result(&result, &arguments)
    }

    fn proof(
        &self,
        lease: &RuntimeCapabilityLeaseV1,
        execution_id: &str,
        body: &Value,
    ) -> Result<String, AttachmentCapabilityError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AttachmentCapabilityError::Configuration)?
            .as_secs();
        let mut fields = Map::new();
        fields.insert(
            "body_sha256".into(),
            Value::String(
                canonical_json_hash(body)
                    .map_err(|_| AttachmentCapabilityError::Arguments)?
                    .strip_prefix("sha256:")
                    .ok_or(AttachmentCapabilityError::Arguments)?
                    .to_string(),
            ),
        );
        fields.insert(
            "execution_id".into(),
            Value::String(execution_id.to_string()),
        );
        fields.insert(
            "expires_at".into(),
            json!(
                now.checked_add(PROOF_TTL_SECONDS)
                    .ok_or(AttachmentCapabilityError::Configuration)?
            ),
        );
        fields.insert("method".into(), Value::String("POST".into()));
        fields.insert(
            "nonce".into(),
            Value::String(Uuid::now_v7().simple().to_string()),
        );
        fields.insert("path".into(), Value::String(PATH.into()));
        fields.insert("run_id".into(), Value::String(lease.run_id.clone()));
        fields.insert("schema_version".into(), Value::String(PROOF_SCHEMA.into()));
        fields.insert("session_id".into(), Value::String(lease.session_id.clone()));
        fields.insert("tenant_id".into(), Value::String(lease.tenant_id.clone()));
        fields.insert("user_id".into(), Value::String(lease.user_id.clone()));
        let unsigned = serde_json::to_vec(&Value::Object(fields.clone()))
            .map_err(|_| AttachmentCapabilityError::Configuration)?;
        let mut mac = HmacSha256::new_from_slice(self.config.proof_secret.as_bytes())
            .map_err(|_| AttachmentCapabilityError::Configuration)?;
        mac.update(&unsigned);
        fields.insert(
            "signature".into(),
            Value::String(hex::encode(mac.finalize().into_bytes())),
        );
        Ok(format!(
            "v1.{}",
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&Value::Object(fields))
                    .map_err(|_| AttachmentCapabilityError::Configuration)?
            )
        ))
    }
}

fn normalize_bound_arguments(
    raw_arguments: Value,
    lease_arguments_hash: &str,
) -> Result<(AttachmentReadArguments, Value), AttachmentCapabilityError> {
    // The Runtime lease hashes the model's original JSON. Deserializing first
    // inserts max_chars=100000 when omitted and must not change that binding.
    let actual_hash =
        canonical_json_hash(&raw_arguments).map_err(|_| AttachmentCapabilityError::Arguments)?;
    if actual_hash != lease_arguments_hash {
        return Err(AttachmentCapabilityError::ArgumentsHashMismatch);
    }
    let arguments: AttachmentReadArguments =
        serde_json::from_value(raw_arguments).map_err(|_| AttachmentCapabilityError::Arguments)?;
    arguments.validate()?;
    let body =
        serde_json::to_value(&arguments).map_err(|_| AttachmentCapabilityError::Arguments)?;
    Ok((arguments, body))
}

fn validate_result(
    result: &AttachmentReadResult,
    arguments: &AttachmentReadArguments,
) -> Result<AttachmentReadResult, AttachmentCapabilityError> {
    if result.attachment_id != arguments.attachment_id
        || result.filename.is_empty()
        || result.filename.len() > 255
        || result.filename.contains('/')
        || result.filename.contains('\\')
        || result
            .filename
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
        || result.sha256.len() != 64
        || result.sha256.bytes().any(|byte| !byte.is_ascii_hexdigit())
        || result
            .content
            .as_ref()
            .is_some_and(|content| content.chars().count() > arguments.max_chars)
        || (arguments.operation == "metadata" && result.content.is_some())
    {
        return Err(AttachmentCapabilityError::MalformedResponse);
    }
    Ok(result.clone())
}

async fn bounded_response(mut response: Response) -> Result<Vec<u8>, AttachmentCapabilityError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(AttachmentCapabilityError::MalformedResponse);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| AttachmentCapabilityError::SideEffectUnknown)?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(AttachmentCapabilityError::MalformedResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_path_like_attachment_ids_and_unbounded_reads() {
        assert!(
            AttachmentReadArguments {
                attachment_id: "../x".into(),
                operation: "content".into(),
                max_chars: 10
            }
            .validate()
            .is_err()
        );
        assert!(
            AttachmentReadArguments {
                attachment_id: "x".into(),
                operation: "content".into(),
                max_chars: MAX_CONTENT_CHARS + 1
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn omitted_max_chars_uses_bounded_default() {
        let raw = serde_json::json!({
            "attachment_id": "art_1111111111111111", "operation": "content"
        });
        let hash = canonical_json_hash(&raw).unwrap();
        let (args, body) = normalize_bound_arguments(raw, &hash).expect("raw lease remains valid");
        assert_eq!(args.max_chars, 100_000);
        assert_eq!(body["max_chars"], 100_000);
        assert!(args.validate().is_ok());
        assert_eq!(
            normalize_bound_arguments(body, &hash).unwrap_err(),
            AttachmentCapabilityError::ArgumentsHashMismatch
        );
    }
    #[test]
    fn endpoint_has_no_caller_controlled_path() {
        let config = AttachmentCapabilityConfig {
            gateway_url: "http://gateway:8080/base".into(),
            internal_token: "x".repeat(32),
            proof_secret: "y".repeat(32),
        };
        assert_eq!(config.endpoint().unwrap().path(), PATH);
    }
}
