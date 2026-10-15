//! Tenant-bound read-only platform capability adapters.

mod executor;
mod fs;
mod proof;
mod web;
mod working_memory;

pub use self::executor::ReadCapabilityExecutor;
pub(crate) use self::working_memory::render_working_memory;
pub(crate) use self::working_memory::validate_working_memory;

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use hmac::Hmac;
use reqwest::{Client, Response, Url};
use serde_json::Value;
use sha2::{Digest, Sha256};

use self::web::{is_public_ip, resolved_peer_ip};
use crate::RuntimeConnectorBinding;

const MAX_INTERNAL_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_MCP_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_WEB_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_REDIRECTS: usize = 3;
const MAX_GLOB_RESULTS: usize = 1_000;
const MAX_GREP_RESULTS: usize = 500;
const MAX_WALKED_FILES: usize = 20_000;
const CAPABILITY_PROOF_SCHEMA_VERSION: &str = "ai-platform-capability-proof/v1";
const CAPABILITY_PROOF_TTL_SECONDS: u64 = 30;
const MIN_PROOF_SECRET_BYTES: usize = 32;
const WORKING_MEMORY_SCHEMA_VERSION: &str = "assistant-working-memory/v2";
const MAX_WORKING_MEMORY_BYTES: usize = 100_000;
const MAX_WORKING_MEMORY_TASKS: usize = 100;
const MAX_WORKING_MEMORY_TASK_ID_BYTES: usize = 128;
const MAX_WORKING_MEMORY_DESCRIPTION_BYTES: usize = 1_000;
const MAX_WORKING_MEMORY_RESULT_BYTES: usize = 500;
const MAX_WORKING_MEMORY_GOAL_BYTES: usize = 2_000;
const MAX_WORKING_MEMORY_INFO_KEY_BYTES: usize = 128;
const MAX_WORKING_MEMORY_INFO_VALUE_BYTES: usize = 2_000;
const MAX_WORKING_MEMORY_INFO_SOURCE_BYTES: usize = 128;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug)]
pub struct ReadCapabilityConfig {
    pub knowledge_base_url: String,
    pub gateway_url: String,
    pub workspace_root: PathBuf,
    pub internal_token: String,
    pub proof_secret: String,
}

impl ReadCapabilityConfig {
    pub fn validate(&self) -> Result<(), ReadCapabilityError> {
        internal_base_url(&self.knowledge_base_url)?;
        internal_base_url(&self.gateway_url)?;
        if !self.workspace_root.is_absolute()
            || self.internal_token.len() < 32
            || self.proof_secret.len() < MIN_PROOF_SECRET_BYTES
        {
            return Err(ReadCapabilityError::Configuration);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadCapabilityContext {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub execution_id: String,
    pub tool_call_id: String,
    pub run_id: String,
    pub capability_revision: u64,
    pub bound_dataset_ids: BTreeSet<String>,
    pub connector_binding: Option<RuntimeConnectorBinding>,
}

impl ReadCapabilityContext {
    fn validate(&self) -> Result<(), ReadCapabilityError> {
        if self.capability_revision == 0
            || [
                self.tenant_id.as_str(),
                self.user_id.as_str(),
                self.session_id.as_str(),
                self.execution_id.as_str(),
                self.tool_call_id.as_str(),
                self.run_id.as_str(),
            ]
            .into_iter()
            .any(|value| {
                value.is_empty()
                    || value.len() > 255
                    || value.bytes().any(|byte| byte.is_ascii_control())
            })
        {
            return Err(ReadCapabilityError::Scope);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ReadCapabilityError {
    #[error("read_capability_configuration_invalid")]
    Configuration,
    #[error("read_capability_scope_invalid")]
    Scope,
    #[error("read_capability_arguments_invalid")]
    Arguments,
    #[error("read_capability_not_found")]
    NotFound,
    #[error("read_capability_path_escape")]
    PathEscape,
    #[error("read_capability_ssrf_blocked")]
    SsrfBlocked,
    #[error("read_capability_dns_failed")]
    DnsFailed,
    #[error("read_capability_response_too_large")]
    ResponseTooLarge,
    #[error("read_capability_downstream_rejected")]
    DownstreamRejected,
    #[error("read_capability_downstream_unavailable")]
    DownstreamUnavailable,
    #[error("read_capability_worker_failed")]
    WorkerFailed,
    #[error("read_capability_working_memory_invalid")]
    WorkingMemoryInvalid,
}

#[derive(Clone, Debug)]
pub struct PublicHttpResponse {
    pub status: u16,
    pub content_type: String,
    pub location: Option<String>,
    pub remote_ip: Option<IpAddr>,
    pub bytes: Vec<u8>,
}

#[async_trait]
pub trait ReadHttpAdapter: Send + Sync {
    async fn post_internal_json(
        &self,
        url: Url,
        headers: &[(String, String)],
        body: Value,
    ) -> Result<Value, ReadCapabilityError>;

    async fn get_public(&self, url: Url) -> Result<PublicHttpResponse, ReadCapabilityError>;
}

/// Read-only access to session memory. The caller supplies the owner-bound
/// v2 key; implementations must not fall back to the legacy unscoped key.
#[async_trait]
pub trait SessionMemoryReadAdapter: Send + Sync {
    async fn get_session_memory(
        &self,
        tenant_id: &str,
        session_id: &str,
        key: &str,
    ) -> Result<Option<Value>, ReadCapabilityError>;
}

#[derive(Clone)]
pub struct PostgresSessionMemoryReadAdapter {
    pool: sqlx::PgPool,
}

impl PostgresSessionMemoryReadAdapter {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> sqlx::PgPool {
        self.pool.clone()
    }
}

#[async_trait]
impl SessionMemoryReadAdapter for PostgresSessionMemoryReadAdapter {
    async fn get_session_memory(
        &self,
        tenant_id: &str,
        session_id: &str,
        key: &str,
    ) -> Result<Option<Value>, ReadCapabilityError> {
        sqlx::query_scalar::<_, Value>(
            "SELECT value FROM session_memory \
             WHERE tenant_id = $1 AND session_id = $2 AND key = $3 \
               AND (expires_at IS NULL OR expires_at > NOW())",
        )
        .bind(tenant_id)
        .bind(session_id)
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| ReadCapabilityError::WorkerFailed)
    }
}

#[derive(Clone)]
pub struct ReqwestReadHttpAdapter {
    client: Client,
}

impl ReqwestReadHttpAdapter {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl ReadHttpAdapter for ReqwestReadHttpAdapter {
    async fn post_internal_json(
        &self,
        url: Url,
        headers: &[(String, String)],
        body: Value,
    ) -> Result<Value, ReadCapabilityError> {
        for attempt in 0..2 {
            let mut request = self.client.post(url.clone()).json(&body);
            for (name, value) in headers {
                request = request.header(name, value);
            }
            let response = match request.send().await {
                Ok(response) => response,
                Err(error) if attempt == 0 && error.is_timeout() => continue,
                Err(_) => return Err(ReadCapabilityError::DownstreamUnavailable),
            };
            if response.status().is_server_error() && attempt == 0 {
                continue;
            }
            if !response.status().is_success() {
                return Err(ReadCapabilityError::DownstreamRejected);
            }
            let bytes = bounded_response(response, MAX_INTERNAL_RESPONSE_BYTES).await?;
            return serde_json::from_slice(&bytes)
                .map_err(|_| ReadCapabilityError::DownstreamRejected);
        }
        Err(ReadCapabilityError::DownstreamUnavailable)
    }

    async fn get_public(&self, url: Url) -> Result<PublicHttpResponse, ReadCapabilityError> {
        let host = url.host_str().ok_or(ReadCapabilityError::SsrfBlocked)?;
        let port = url
            .port_or_known_default()
            .ok_or(ReadCapabilityError::SsrfBlocked)?;
        let addresses = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| ReadCapabilityError::DnsFailed)?
            .collect::<Vec<_>>();
        if addresses.is_empty() || addresses.iter().any(|address| !is_public_ip(address.ip())) {
            return Err(ReadCapabilityError::SsrfBlocked);
        }
        // Disable environment proxies and pin this request to the validated
        // addresses. Otherwise a proxy can turn a public peer connection into
        // an internal fetch, or DNS can change between validation and connect.
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .resolve_to_addrs(host, &addresses)
            .build()
            .map_err(|_| ReadCapabilityError::Configuration)?;
        let response = client
            .get(url)
            .header(
                reqwest::header::USER_AGENT,
                "Mozilla/5.0 (compatible; AI-Gateway-web_fetch/2.0)",
            )
            .header(
                reqwest::header::ACCEPT,
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .send()
            .await
            .map_err(|_| ReadCapabilityError::DownstreamUnavailable)?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .chars()
            .take(128)
            .collect();
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let remote_ip = resolved_peer_ip(
            response.remote_addr().map(|address| address.ip()),
            &addresses,
        );
        let bytes = bounded_response(response, MAX_WEB_RESPONSE_BYTES).await?;
        Ok(PublicHttpResponse {
            status,
            content_type,
            location,
            remote_ip,
            bytes,
        })
    }
}

async fn bounded_response(
    mut response: Response,
    limit: usize,
) -> Result<Vec<u8>, ReadCapabilityError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(ReadCapabilityError::ResponseTooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ReadCapabilityError::DownstreamUnavailable)?
    {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(ReadCapabilityError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// The v2 key is intentionally byte-for-byte compatible with Python's
/// `_scope_digest`: each UTF-8 component is length-prefixed as an unsigned
/// big-endian u64 before being hashed.
pub fn working_memory_key(tenant_id: &str, user_id: &str, session_id: &str) -> String {
    let mut digest = Sha256::new();
    for value in [tenant_id, user_id, session_id] {
        let bytes = value.as_bytes();
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    format!("working_memory:{:x}", digest.finalize())
}

fn internal_base_url(value: &str) -> Result<Url, ReadCapabilityError> {
    let url = Url::parse(value).map_err(|_| ReadCapabilityError::Configuration)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ReadCapabilityError::Configuration);
    }
    Ok(url)
}

fn internal_endpoint(base: &str, path: &str) -> Result<Url, ReadCapabilityError> {
    let mut base = internal_base_url(base)?;
    if !base.path().ends_with('/') {
        base.set_path(&format!("{}/", base.path()));
    }
    base.join(path)
        .map_err(|_| ReadCapabilityError::Configuration)
}
