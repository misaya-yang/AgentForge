//! Single-attempt Confluence write broker client.
//!
//! The capability worker sends only the validated arguments and connector
//! binding to the Gateway.  Credentials are resolved by the Gateway after it
//! verifies the capability proof; no token is accepted in this request body.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ai_platform_capability_contract::canonical_json_hash;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use reqwest::{Client, Response, Url};
use serde_json::{Value, json};
use sha2::Sha256;
use uuid::Uuid;

use crate::external_write_capabilities::{
    GatewayBrokerError, GatewayWriteBroker, GatewayWriteRequest, GatewayWriteResponse,
};

const PATH: &str = "/internal/v2/agent-capabilities/confluence/write";
const PROOF_SCHEMA: &str = "ai-platform-capability-proof/v1";
const PROOF_TTL_SECONDS: u64 = 30;
const MIN_SECRET_BYTES: usize = 32;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug)]
pub struct ConfluenceWriteBrokerConfig {
    pub gateway_url: String,
    pub internal_token: String,
    pub proof_secret: String,
}

impl ConfluenceWriteBrokerConfig {
    fn endpoint(&self) -> Result<Url, GatewayBrokerError> {
        let mut base =
            Url::parse(&self.gateway_url).map_err(|_| GatewayBrokerError::Unavailable)?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || self.internal_token.len() < MIN_SECRET_BYTES
            || self.proof_secret.len() < MIN_SECRET_BYTES
        {
            return Err(GatewayBrokerError::Unavailable);
        }
        base.set_path(PATH);
        Ok(base)
    }
}

#[derive(Clone)]
pub struct ReqwestConfluenceWriteBroker {
    client: Client,
    config: ConfluenceWriteBrokerConfig,
}

impl ReqwestConfluenceWriteBroker {
    pub fn new(
        client: Client,
        config: ConfluenceWriteBrokerConfig,
    ) -> Result<Self, GatewayBrokerError> {
        config.endpoint()?;
        Ok(Self { client, config })
    }

    fn envelope(request: &GatewayWriteRequest) -> Result<Value, GatewayBrokerError> {
        if request.capability_id != "confluence_write"
            || request.connector_binding.is_none()
            || request.connector_binding.as_ref().is_some_and(|binding| {
                binding.provider != "confluence" || binding.tool_name != "confluence_write"
            })
        {
            return Err(GatewayBrokerError::MalformedResponse);
        }
        Ok(json!({
            "arguments": &request.arguments,
            "arguments_hash": &request.arguments_hash,
            "binding": &request.connector_binding,
        }))
    }

    fn proof(
        &self,
        request: &GatewayWriteRequest,
        body: &Value,
        now: u64,
    ) -> Result<String, GatewayBrokerError> {
        if now == 0 || self.config.proof_secret.len() < MIN_SECRET_BYTES {
            return Err(GatewayBrokerError::Unavailable);
        }
        let expires_at = now
            .checked_add(PROOF_TTL_SECONDS)
            .ok_or(GatewayBrokerError::Unavailable)?;
        let body_sha256 = canonical_json_hash(body)
            .map_err(|_| GatewayBrokerError::MalformedResponse)?
            .strip_prefix("sha256:")
            .ok_or(GatewayBrokerError::MalformedResponse)?
            .to_string();
        let mut unsigned = BTreeMap::<String, Value>::new();
        unsigned.insert("body_sha256".into(), Value::String(body_sha256));
        unsigned.insert(
            "execution_id".into(),
            Value::String(request.execution_id.clone()),
        );
        unsigned.insert("expires_at".into(), json!(expires_at));
        unsigned.insert("method".into(), Value::String("POST".into()));
        unsigned.insert(
            "nonce".into(),
            Value::String(Uuid::now_v7().simple().to_string()),
        );
        unsigned.insert("path".into(), Value::String(PATH.into()));
        unsigned.insert("run_id".into(), Value::String(request.run_id.clone()));
        unsigned.insert("schema_version".into(), Value::String(PROOF_SCHEMA.into()));
        unsigned.insert(
            "session_id".into(),
            Value::String(request.session_id.clone()),
        );
        unsigned.insert("tenant_id".into(), Value::String(request.tenant_id.clone()));
        unsigned.insert("user_id".into(), Value::String(request.user_id.clone()));
        let bytes = serde_json::to_vec(&unsigned).map_err(|_| GatewayBrokerError::Unavailable)?;
        let mut mac = HmacSha256::new_from_slice(self.config.proof_secret.as_bytes())
            .map_err(|_| GatewayBrokerError::Unavailable)?;
        mac.update(&bytes);
        let signature = hex::encode(mac.finalize().into_bytes());
        unsigned.insert("signature".into(), Value::String(signature));
        let envelope =
            serde_json::to_vec(&unsigned).map_err(|_| GatewayBrokerError::Unavailable)?;
        Ok(format!("v1.{}", URL_SAFE_NO_PAD.encode(envelope)))
    }
}

#[async_trait]
impl GatewayWriteBroker for ReqwestConfluenceWriteBroker {
    async fn execute_once(
        &self,
        request: GatewayWriteRequest,
    ) -> Result<GatewayWriteResponse, GatewayBrokerError> {
        let body = Self::envelope(&request)?;
        let endpoint = self.config.endpoint()?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| GatewayBrokerError::Unavailable)?
            .as_secs();
        let proof = self.proof(&request, &body, now)?;
        let request_builder = self
            .client
            .post(endpoint)
            .header("x-ai-platform-internal-token", &self.config.internal_token)
            .header("x-ai-tenant-id", &request.tenant_id)
            .header("x-ai-user-id", &request.user_id)
            .header("x-ai-session-id", &request.session_id)
            .header("x-ai-tool-call-id", &request.tool_call_id)
            .header("x-ai-capability-proof", proof);
        let response = crate::trace_context::apply(
            request_builder,
            &request.run_id,
            &request.run_id,
            &request.execution_id,
        )
        .json(&body)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                GatewayBrokerError::Timeout
            } else {
                GatewayBrokerError::Unavailable
            }
        })?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(GatewayBrokerError::HttpStatus(status));
        }
        let bytes = bounded_response(response).await?;
        serde_json::from_slice(&bytes).map_err(|_| GatewayBrokerError::MalformedResponse)
    }
}

async fn bounded_response(mut response: Response) -> Result<Vec<u8>, GatewayBrokerError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(GatewayBrokerError::MalformedResponse);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| GatewayBrokerError::Unavailable)?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(GatewayBrokerError::MalformedResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::{GatewayBrokerError, PATH, ReqwestConfluenceWriteBroker};
    use crate::external_write_capabilities::ExternalWriteError;
    use serde_json::json;

    #[test]
    fn envelope_contains_no_credential_and_uses_fixed_path() {
        let request = super::GatewayWriteRequest {
            tenant_id: "tenant".into(),
            user_id: "user".into(),
            session_id: "session".into(),
            execution_id: "execution".into(),
            run_id: "run".into(),
            tool_call_id: "call".into(),
            capability_id: "confluence_write".into(),
            arguments_hash: "sha256:hash".into(),
            arguments: json!({"action": "delete_page", "page_id": "42"}),
            connector_binding: Some(crate::RuntimeConnectorBinding {
                binding_type: "catalog".into(),
                provider: "confluence".into(),
                tool_name: "confluence_write".into(),
                principal_type: None,
                grant_id: None,
                connection_id: None,
                schema_hash: None,
                risk_level: None,
                channel: "api".into(),
            }),
        };
        let body = ReqwestConfluenceWriteBroker::envelope(&request).expect("valid envelope");
        assert_eq!(PATH, "/internal/v2/agent-capabilities/confluence/write");
        assert!(body.get("api_token").is_none());
        assert!(body.get("secret").is_none());
    }

    #[test]
    fn upstream_5xx_is_side_effect_unknown_but_4xx_is_definitive() {
        assert_eq!(
            ExternalWriteError::from(GatewayBrokerError::HttpStatus(503)),
            ExternalWriteError::SideEffectUnknown
        );
        assert_eq!(
            ExternalWriteError::from(GatewayBrokerError::HttpStatus(409)),
            ExternalWriteError::Failed
        );
    }
}
