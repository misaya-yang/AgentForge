//! Single-attempt Gateway broker for Office artifact persistence.
//!
//! Office rendering happens in the worker, while the Gateway remains the only
//! process allowed to use the configured object store.  This client sends
//! bounded bytes plus a scope-bound capability proof and never retries a
//! request whose upload outcome is uncertain.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ai_platform_capability_contract::canonical_json_hash;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use reqwest::{Client, Response, Url};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::office_capabilities::{
    OfficeArtifactPut, OfficeArtifactRecord, OfficeArtifactStore, OfficeArtifactStoreError,
};

const PATH: &str = "/internal/v2/agent-capabilities/office/artifacts";
const PROOF_SCHEMA: &str = "ai-platform-capability-proof/v1";
const PROOF_TTL_SECONDS: u64 = 30;
const MIN_SECRET_BYTES: usize = 32;
const MAX_CONTENT_BYTES: usize = 32 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug)]
pub struct OfficeArtifactBrokerConfig {
    pub gateway_url: String,
    pub internal_token: String,
    pub proof_secret: String,
}

impl OfficeArtifactBrokerConfig {
    fn endpoint(&self) -> Result<Url, OfficeArtifactStoreError> {
        let mut url =
            Url::parse(&self.gateway_url).map_err(|_| OfficeArtifactStoreError::Unavailable)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || self.internal_token.len() < MIN_SECRET_BYTES
            || self.proof_secret.len() < MIN_SECRET_BYTES
        {
            return Err(OfficeArtifactStoreError::Unavailable);
        }
        url.set_path(PATH);
        Ok(url)
    }
}

#[derive(Clone)]
pub struct ReqwestOfficeArtifactStore {
    client: Client,
    config: OfficeArtifactBrokerConfig,
}

impl ReqwestOfficeArtifactStore {
    pub fn new(
        client: Client,
        config: OfficeArtifactBrokerConfig,
    ) -> Result<Self, OfficeArtifactStoreError> {
        config.endpoint()?;
        Ok(Self { client, config })
    }

    fn body(request: &OfficeArtifactPut) -> Result<Value, OfficeArtifactStoreError> {
        if request.content.is_empty()
            || request.content.len() > MAX_CONTENT_BYTES
            || request.sha256.len() != 64
            || !request.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || request.arguments_hash.len() != 71
            || !request.arguments_hash.starts_with("sha256:")
        {
            return Err(OfficeArtifactStoreError::InvalidMetadata);
        }
        let actual = hex::encode(Sha256::digest(&request.content));
        if actual != request.sha256 {
            return Err(OfficeArtifactStoreError::InvalidMetadata);
        }
        Ok(json!({
            "tool_call_id": request.tool_call_id,
            "arguments_hash": request.arguments_hash,
            "artifact_type": request.artifact_type,
            "format": request.format,
            "title": request.title,
            "filename": request.filename,
            "mime_type": request.mime_type,
            "sha256": request.sha256,
            "size_bytes": request.content.len(),
            "content_base64": STANDARD.encode(&request.content),
            "metadata": request.metadata,
        }))
    }

    fn proof(
        &self,
        request: &OfficeArtifactPut,
        body: &Value,
    ) -> Result<String, OfficeArtifactStoreError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| OfficeArtifactStoreError::Unavailable)?
            .as_secs();
        let expires_at = now
            .checked_add(PROOF_TTL_SECONDS)
            .ok_or(OfficeArtifactStoreError::Unavailable)?;
        let body_hash = canonical_json_hash(body)
            .map_err(|_| OfficeArtifactStoreError::InvalidMetadata)?
            .strip_prefix("sha256:")
            .ok_or(OfficeArtifactStoreError::InvalidMetadata)?
            .to_owned();
        let mut unsigned = BTreeMap::<String, Value>::new();
        for (key, value) in [
            ("body_sha256", Value::String(body_hash)),
            ("execution_id", Value::String(request.execution_id.clone())),
            ("expires_at", json!(expires_at)),
            ("method", Value::String("POST".into())),
            ("nonce", Value::String(Uuid::now_v7().simple().to_string())),
            ("path", Value::String(PATH.into())),
            ("run_id", Value::String(request.run_id.clone())),
            ("schema_version", Value::String(PROOF_SCHEMA.into())),
            ("session_id", Value::String(request.session_id.clone())),
            ("tenant_id", Value::String(request.tenant_id.clone())),
            ("user_id", Value::String(request.user_id.clone())),
        ] {
            unsigned.insert(key.into(), value);
        }
        let bytes =
            serde_json::to_vec(&unsigned).map_err(|_| OfficeArtifactStoreError::Unavailable)?;
        let mut mac = HmacSha256::new_from_slice(self.config.proof_secret.as_bytes())
            .map_err(|_| OfficeArtifactStoreError::Unavailable)?;
        mac.update(&bytes);
        unsigned.insert(
            "signature".into(),
            Value::String(hex::encode(mac.finalize().into_bytes())),
        );
        Ok(format!(
            "v1.{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&unsigned).map_err(|_| OfficeArtifactStoreError::Unavailable)?
            )
        ))
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GatewayArtifactResponse {
    artifact_id: String,
    #[serde(alias = "download_path")]
    download_url: String,
    filename: String,
    mime_type: String,
    size_bytes: u64,
    sha256: String,
}

#[async_trait]
impl OfficeArtifactStore for ReqwestOfficeArtifactStore {
    async fn put_idempotent(
        &self,
        request: OfficeArtifactPut,
    ) -> Result<OfficeArtifactRecord, OfficeArtifactStoreError> {
        let body = Self::body(&request)?;
        let proof = self.proof(&request, &body)?;
        let request_builder = self
            .client
            .post(self.config.endpoint()?)
            .header("x-ai-platform-internal-token", &self.config.internal_token)
            .header("x-ai-tenant-id", &request.tenant_id)
            .header("x-ai-user-id", &request.user_id)
            .header("x-ai-session-id", &request.session_id)
            .header("x-ai-tool-call-id", &request.tool_call_id)
            .header("x-ai-arguments-hash", &request.arguments_hash)
            .header("x-ai-capability-proof", proof);
        let response = crate::trace_context::apply(
            request_builder,
            &request.run_id,
            &request.run_id,
            &request.execution_id,
        )
        .json(&body)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                OfficeArtifactStoreError::OutcomeUnknown
            } else {
                OfficeArtifactStoreError::Unavailable
            }
        })?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            return Err(if (400..500).contains(&status) {
                OfficeArtifactStoreError::IdempotencyConflict
            } else {
                OfficeArtifactStoreError::OutcomeUnknown
            });
        }
        let bytes = bounded_response(response).await?;
        let value: GatewayArtifactResponse =
            serde_json::from_slice(&bytes).map_err(|_| OfficeArtifactStoreError::OutcomeUnknown)?;
        if value.artifact_id.is_empty()
            || value.download_url.is_empty()
            || value.filename != request.filename
            || value.mime_type != request.mime_type
            || value.size_bytes != request.content.len() as u64
            || value.sha256 != request.sha256
        {
            return Err(OfficeArtifactStoreError::InvalidMetadata);
        }
        Ok(OfficeArtifactRecord {
            artifact_id: value.artifact_id,
            download_url: value.download_url,
            filename: value.filename,
            mime_type: value.mime_type,
            size_bytes: value.size_bytes,
            sha256: value.sha256,
        })
    }
}

async fn bounded_response(mut response: Response) -> Result<Vec<u8>, OfficeArtifactStoreError> {
    if response
        .content_length()
        .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
    {
        return Err(OfficeArtifactStoreError::OutcomeUnknown);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| OfficeArtifactStoreError::OutcomeUnknown)?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(OfficeArtifactStoreError::OutcomeUnknown);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_untrusted_endpoint_and_invalid_content() {
        let config = OfficeArtifactBrokerConfig {
            gateway_url: "file:///tmp".into(),
            internal_token: "x".repeat(32),
            proof_secret: "y".repeat(32),
        };
        assert!(matches!(
            config.endpoint(),
            Err(OfficeArtifactStoreError::Unavailable)
        ));
    }
}
