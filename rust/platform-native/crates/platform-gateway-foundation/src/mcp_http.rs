//! Native, opt-in MCP discovery transport. No invocation or OAuth exchange.
//!
//! Constructors are inert. A polled factory validates all DNS answers and pins
//! exactly the smallest IPv4-then-IPv6 address, retaining the original URL for
//! Host and certificate/SNI verification. No redirect, proxy, automatic retry,
//! private-network exception, or second DNS lookup is permitted. A generation
//! retains this pin for initialize, initialized and tools/list.
//!
//! Boundaries: reqwest owns TLS roots, URL normalization and transport shutdown
//! (Drop is not an awaited httpx.aclose). Its connect/read timers cannot represent
//! independent httpx write/pool timers. JSON is the serde finite-number/UTF-8
//! domain, not Python arbitrary integers, nonfinite numbers or UTF-16 JSON.
//! Python's last `data:` line semantics are intentional, not general SSE framing.
//! Every returned failure is a fixed code; no upstream body, URL or secret leaks.

use crate::mcp_registry_store::{McpError, McpFuture};
use crate::service_registry::python_truthy;
use crate::usage_parser::python_value_to_string;
use reqwest::{
    Client, Url,
    header::{HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use std::{
    fmt,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    sync::{Arc, Mutex},
    time::Duration,
};

#[path="mcp_http/revision.rs"]
pub mod revision;
#[path="mcp_http/oauth.rs"]
pub mod oauth;
#[path="mcp_http/initialize.rs"]
mod initialize;

pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
pub const MCP_MAX_TOOLS: usize = 50;

pub(crate) fn broker_error(code: &str) -> McpError {
    McpError::Broker(code.into())
}
pub(crate) fn record_integer(record: &Value, key: &str, default: i64) -> Result<i64, McpError> {
    let Some(value) = record.get(key).filter(|v| python_truthy(v)) else {
        return Ok(default);
    };
    match value {
        Value::Bool(v) => Ok(i64::from(*v)),
        Value::Number(v) => v
            .as_i64()
            .or_else(|| {
                v.as_f64()
                    .filter(|f| f.is_finite() && *f >= i64::MIN as f64 && *f < -(i64::MIN as f64))
                    .map(|f| f.trunc() as i64)
            })
            .ok_or(McpError::Unhandled),
        Value::String(v) => {
            let v = v.trim();
            let bytes = v.as_bytes();
            if bytes.iter().enumerate().any(|(i, b)| {
                *b == b'_'
                    && (i == 0
                        || !bytes[i - 1].is_ascii_digit()
                        || !bytes.get(i + 1).is_some_and(u8::is_ascii_digit))
            }) {
                return Err(McpError::Unhandled);
            }
            v.replace('_', "").parse().map_err(|_| McpError::Unhandled)
        }
        _ => Err(McpError::Unhandled),
    }
}
fn transport_error(error: reqwest::Error) -> McpError {
    broker_error(if error.is_timeout() {
        "MCP_TIMEOUT"
    } else {
        "MCP_UPSTREAM_UNAVAILABLE"
    })
}

/// Secret-bearing configuration deliberately has no field-wise Debug output.
#[derive(Clone)]
pub struct McpClientConfig {
    pub base_url: String,
    pub revision: revision::Revision,
    pub oauth_metadata_url: Option<String>,
    pub endpoint_path: String,
    pub timeout: Duration,
    pub max_concurrency: usize,
    pub response_limit_bytes: usize,
    pub auth_method: String,
    pub oauth_resource: Option<String>,
    pub oauth_audience: Option<String>,
    pub credential_audience: Option<String>,
    pub origin: Option<String>,
    pub allowed_origins: Vec<String>,
    credential: Option<String>,
}
impl fmt::Debug for McpClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("McpClientConfig(<redacted>)")
    }
}
impl McpClientConfig {
    pub fn from_record(record: &Value, credential: Option<String>) -> Result<Self, McpError> {
        if record.get("transport").and_then(Value::as_str).is_some_and(|t|t!="streamable_http"){return Err(broker_error("MCP_STDIO_SANDBOX_REQUIRED"))}
        let text = |key: &str| record.get(key).and_then(Value::as_str).map(str::to_owned);
        Ok(Self {
            revision:revision::Revision::configured(record).map_err(broker_error)?,
            oauth_metadata_url:text("oauth_metadata_url"),
            base_url: text("base_url").ok_or_else(|| broker_error("MCP_URL_INVALID"))?,
            endpoint_path: text("endpoint_path")
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "/mcp".into()),
            timeout: Duration::from_millis(
                record_integer(record, "timeout_ms", 30_000)?.max(100) as u64
            ),
            max_concurrency: record_integer(record, "max_concurrency", 5)?.clamp(1, 32) as usize,
            response_limit_bytes: record_integer(record, "response_limit_bytes", 1_048_576)?
                .clamp(1024, 8_388_608) as usize,
            auth_method: text("auth_method")
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "none".into()),
            oauth_resource: text("oauth_resource"),
            oauth_audience: text("oauth_audience"),
            credential_audience: text("audience"),
            origin: None,
            allowed_origins: record
                .get("allowed_origins")
                .and_then(Value::as_array)
                .map(|v| v.iter().map(python_value_to_string).collect())
                .unwrap_or_default(),
            credential,
        })
    }
    fn validate(&self) -> Result<Url, McpError> {
        let url = Url::parse(&self.base_url).map_err(|_| broker_error("MCP_URL_INVALID"))?;
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || self
                .base_url
                .split_once("://")
                .map(|(_, rest)| {
                    rest.split(['/', '?', '#'])
                        .next()
                        .unwrap_or("")
                        .contains('@')
                })
                .unwrap_or(false)
        {
            return Err(broker_error("MCP_URL_INVALID"));
        }
        if url.scheme() != "https" {
            return Err(broker_error("MCP_TLS_REQUIRED"));
        }
        if !self.endpoint_path.starts_with('/')
            || self.endpoint_path.contains("..")
            || self.timeout.is_zero()
            || !(1..=32).contains(&self.max_concurrency)
            || !(1024..=8_388_608).contains(&self.response_limit_bytes)
        {
            return Err(broker_error("MCP_CONFIG_INVALID"));
        }
        if self.auth_method == "oauth"
            && (self.oauth_resource.as_deref().unwrap_or("").is_empty()
                || self.oauth_audience.as_deref().unwrap_or("").is_empty()
                || self.credential_audience != self.oauth_audience)
        {
            return Err(broker_error("MCP_OAUTH_AUDIENCE_MISMATCH"));
        }
        if self.revision.modern()&&self.auth_method=="oauth"&&self.oauth_resource!=self.oauth_audience{return Err(broker_error("MCP_OAUTH_RESOURCE_MISMATCH"))}
        if let Some(origin) = self.origin.as_deref().filter(|s| !s.is_empty()) {
            let parsed = Url::parse(origin).map_err(|_| broker_error("MCP_ORIGIN_DENIED"))?;
            if parsed.scheme() != "https"
                || !matches!(parsed.path(), "" | "/")
                || parsed.query().is_some()
                || parsed.fragment().is_some()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || !self
                    .allowed_origins
                    .iter()
                    .any(|v| v.trim_end_matches('/') == origin.trim_end_matches('/'))
            {
                return Err(broker_error("MCP_ORIGIN_DENIED"));
            }
        }
        Ok(url)
    }
}

pub trait McpDnsResolver: Send + Sync {
    fn resolve<'a>(&'a self, hostname: &'a str, port: u16) -> McpFuture<'a, Vec<IpAddr>>;
}
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeMcpDnsResolver;
impl McpDnsResolver for NativeMcpDnsResolver {
    fn resolve<'a>(&'a self, hostname: &'a str, port: u16) -> McpFuture<'a, Vec<IpAddr>> {
        Box::pin(async move {
            let hostname = hostname.to_owned();
            // Like Python asyncio.to_thread(getaddrinfo), cancellation cannot
            // stop an OS lookup already executing. It never performs an HTTP call.
            tokio::task::spawn_blocking(move || {
                (hostname.as_str(), port)
                    .to_socket_addrs()
                    .map(|v| v.map(|s| s.ip()).collect::<Vec<_>>())
            })
            .await
            .map_err(|_| broker_error("MCP_DNS_UNAVAILABLE"))?
            .map_err(|_| broker_error("MCP_DNS_UNAVAILABLE"))
        })
    }
}

/// Python 3.12.14 global tables plus the client's explicit reserved/multicast
/// exclusion. IPv4-mapped IPv6 is denied as reserved by this protocol client.
pub fn mcp_public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let n = u32::from(ip);
            if matches!(n, 0xc0000009 | 0xc000000a) {
                return true;
            }
            ![
                (0u32, 8),
                (0x0a000000, 8),
                (0x64400000, 10),
                (0x7f000000, 8),
                (0xa9fe0000, 16),
                (0xac100000, 12),
                (0xc0000000, 24),
                (0xc0000200, 24),
                (0xc0a80000, 16),
                (0xc6120000, 15),
                (0xc6336400, 24),
                (0xcb007100, 24),
                (0xe0000000, 4),
                (0xf0000000, 4),
            ]
            .iter()
            .any(|&(base, bits)| n >> (32 - bits) == base >> (32 - bits))
        }
        IpAddr::V6(ip) => {
            let n = u128::from(ip);
            let has = |base: u128, bits: u32| n >> (128 - bits) == base >> (128 - bits);
            // Python marks addresses outside these non-reserved prefixes as
            // reserved. A global-looking v4 mapping must not bypass that check.
            if n >> 125 != 1 {
                return false;
            } // 2000::/3
            let exception = has((0x20010001u128 << 96) | 1, 128)
                || has((0x20010001u128 << 96) | 2, 128)
                || has(0x20010003u128 << 96, 32)
                || has(0x200100040112u128 << 80, 48)
                || has(0x20010020u128 << 96, 28)
                || has(0x20010030u128 << 96, 28);
            exception
                || ![
                    (0x20010000u128 << 96, 23),
                    (0x20010db8u128 << 96, 32),
                    (0x2002u128 << 112, 16),
                    (0x3fffu128 << 112, 20),
                ]
                .iter()
                .any(|&(base, bits)| has(base, bits))
        }
    }
}
pub fn select_pinned_address(addresses: &[IpAddr]) -> Result<IpAddr, McpError> {
    if addresses.is_empty() {
        return Err(broker_error("MCP_DNS_UNAVAILABLE"));
    }
    if addresses.iter().any(|v| !mcp_public_address(*v)) {
        return Err(broker_error("MCP_SSRF_BLOCKED"));
    }
    // IpAddr ordering is V4 before V6, then numeric address, matching source.
    addresses
        .iter()
        .copied()
        .min()
        .ok_or_else(|| broker_error("MCP_DNS_UNAVAILABLE"))
}

pub struct McpHttpRequest {
    pub headers: HeaderMap,
    pub payload: Value,
    pub notification: bool,
    session: Arc<Mutex<Option<String>>>,
}
pub struct McpHttpResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}
/// Inject only trusted transport implementations. The native one is below; no
/// arbitrary reqwest client can bypass its pin, proxy or redirect policy.
pub trait McpTransport: Send + Sync {
    fn post(&self, request: McpHttpRequest) -> McpFuture<'_, McpHttpResponse>;
}
pub trait McpTransportFactory: Send + Sync {
    fn connect(
        &self,
        config: McpClientConfig,
        address: IpAddr,
    ) -> McpFuture<'_, Arc<dyn McpTransport>>;
}
#[derive(Clone, Copy, Debug, Default)]
pub struct ReqwestMcpTransportFactory;
struct ReqwestMcpTransport {
    client: Client,
    endpoint: Url,
    limit: usize,
    revision:revision::Revision,
    oauth_resource:Option<String>,
    timeout:Duration,
}
impl McpTransportFactory for ReqwestMcpTransportFactory {
    fn connect(
        &self,
        config: McpClientConfig,
        address: IpAddr,
    ) -> McpFuture<'_, Arc<dyn McpTransport>> {
        Box::pin(async move {
            let (endpoint, host, port) = transport_endpoint(&config)?;
            if !mcp_public_address(address) {
                return Err(broker_error("MCP_SSRF_BLOCKED"));
            }
            if host
                .parse::<IpAddr>()
                .is_ok_and(|literal| literal != address)
            {
                return Err(broker_error("MCP_DNS_REBINDING_BLOCKED"));
            }
            let client = pinned_client_builder(&host, port, address, config.timeout)
                .build()
                .map_err(transport_error)?;
            if config.auth_method=="oauth"&&(config.revision.modern()||config.oauth_metadata_url.is_some()){oauth::fetch(&client,&endpoint,config.oauth_metadata_url.as_deref(),config.oauth_resource.as_deref().ok_or_else(||broker_error("MCP_OAUTH_RESOURCE_MISMATCH"))?,config.timeout).await.map_err(broker_error)?;}
            Ok(Arc::new(ReqwestMcpTransport {
                client,
                endpoint,
                limit: config.response_limit_bytes,
                revision:config.revision,
                oauth_resource:if config.auth_method=="oauth"{config.oauth_resource.clone()}else{None},
                timeout:config.timeout,
            }) as Arc<dyn McpTransport>)
        })
    }
}
fn transport_endpoint(config: &McpClientConfig) -> Result<(Url, String, u16), McpError> {
    let mut endpoint = config.validate()?;
    let host = endpoint
        .host_str()
        .ok_or_else(|| broker_error("MCP_URL_INVALID"))?
        .trim_matches(['[', ']'])
        .to_owned();
    let port = endpoint
        .port_or_known_default()
        .ok_or_else(|| broker_error("MCP_URL_INVALID"))?;
    let relative = config.endpoint_path.split('#').next().unwrap_or("");
    let (path, query) = relative
        .split_once('?')
        .map(|(p, q)| (p, Some(q)))
        .unwrap_or((relative, None));
    let path = format!(
        "{}/{}",
        endpoint.path().trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    endpoint.set_path(&path);
    endpoint.set_query(query);
    Ok((endpoint, host, port))
}
fn pinned_client_builder(
    host: &str,
    port: u16,
    address: IpAddr,
    timeout: Duration,
) -> reqwest::ClientBuilder {
    Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(timeout)
        .read_timeout(timeout)
        .resolve(host, SocketAddr::new(address, port))
        .connection_verbose(false)
}
impl McpTransport for ReqwestMcpTransport {
    fn post(&self, request: McpHttpRequest) -> McpFuture<'_, McpHttpResponse> {
        Box::pin(async move {
            let mut response = self
                .client
                .post(self.endpoint.clone())
                .headers(request.headers)
                .json(&request.payload)
                .send()
                .await
                .map_err(transport_error)?;
            let status = response.status().as_u16();
            if status==401{if let Some(resource)=&self.oauth_resource{let challenge=response_header(response.headers(),"www-authenticate").map(|h|oauth::challenge_url(&h)).transpose().map_err(broker_error)?.flatten();oauth::fetch(&self.client,&self.endpoint,challenge.as_deref(),resource,self.timeout).await.map_err(broker_error)?;return Err(broker_error("MCP_OAUTH_REAUTHORIZATION_REQUIRED"));}}
            if !(self.revision.modern()&&status==400){validate_status(status, request.notification)?;}
            self.revision.check_session(response.headers()).map_err(broker_error)?;
            let headers = response.headers().clone();
            if !self.revision.modern(){pin_session(&request.session, &headers)?;}
            validate_content_length(&headers, self.limit)?;
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
                if chunk.len() > self.limit.saturating_sub(body.len()) {
                    return Err(broker_error("MCP_RESPONSE_TOO_LARGE"));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(McpHttpResponse {
                status,
                headers,
                body,
            })
        })
    }
}
fn validate_status(status: u16, notification: bool) -> Result<(), McpError> {
    if (300..400).contains(&status) {
        return Err(broker_error("MCP_REDIRECT_BLOCKED"));
    }
    if status >= 500 {
        return Err(broker_error("MCP_UPSTREAM_UNAVAILABLE"));
    }
    if (notification && !matches!(status, 200 | 202 | 204))
        || (!notification && !(200..300).contains(&status))
    {
        return Err(broker_error("MCP_UPSTREAM_REJECTED"));
    }
    Ok(())
}
fn validate_content_length(headers: &HeaderMap, limit: usize) -> Result<(), McpError> {
    if let Some(value) = response_header(headers, "content-length")
        && !value.is_empty()
    {
        // HTTP grammar normally excludes signed/underscored values; parsing
        // before allocation also makes malformed lengths fail closed.
        let length = value
            .trim()
            .parse::<u64>()
            .map_err(|_| broker_error("MCP_RESPONSE_INVALID"))?;
        if length > limit as u64 {
            return Err(broker_error("MCP_RESPONSE_TOO_LARGE"));
        }
    }
    Ok(())
}
/// httpx HeaderMap.get joins repeated fields with comma-space. In particular,
/// accepting only the first session field would permit session confusion.
fn response_header(headers: &HeaderMap, name: &str) -> Option<String> {
    let values: Vec<_> = headers
        .get_all(name)
        .iter()
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .collect();
    (!values.is_empty()).then(|| values.join(", "))
}

pub trait McpProtocolClient: Send + Sync {
    fn list_tools(&self) -> McpFuture<'_, Value>;
    /// Synchronous native Drop shutdown; broker never calls this while leased.
    fn close(&self);
}
pub trait McpClientFactory: Send + Sync {
    /// Return only after initialize and notifications/initialized succeeded.
    fn create(&self, config: McpClientConfig) -> McpFuture<'_, McpInitializedClient>;
}
pub struct McpInitializedClient {
    pub client: Arc<dyn McpProtocolClient>,
    pub protocol_version: Option<String>,
}
pub struct NativeMcpClientFactory {
    dns: Arc<dyn McpDnsResolver>,
    transport: Arc<dyn McpTransportFactory>,
}
impl fmt::Debug for NativeMcpClientFactory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeMcpClientFactory(<internal>)")
    }
}
impl NativeMcpClientFactory {
    pub fn new(dns: Arc<dyn McpDnsResolver>, transport: Arc<dyn McpTransportFactory>) -> Self {
        Self { dns, transport }
    }
    pub fn native() -> Self {
        Self::new(
            Arc::new(NativeMcpDnsResolver),
            Arc::new(ReqwestMcpTransportFactory),
        )
    }
}
impl McpClientFactory for NativeMcpClientFactory {
    fn create(&self, config: McpClientConfig) -> McpFuture<'_, McpInitializedClient> {
        Box::pin(async move {
            let modern=config.revision.modern();
            let initialize = async {
                let url = config.validate()?;
                let host = url
                    .host_str()
                    .ok_or_else(|| broker_error("MCP_URL_INVALID"))?
                    .trim_matches(['[', ']']);
                let port = url
                    .port_or_known_default()
                    .ok_or_else(|| broker_error("MCP_URL_INVALID"))?;
                let addresses = self.dns.resolve(host, port).await?;
                let address = select_pinned_address(&addresses)?;
                let wire = self.transport.connect(config.clone(), address).await?;
                let client = Arc::new(McpSession {
                    config,
                    wire: Mutex::new(Some(wire)),
                    session: Arc::new(Mutex::new(None)),
                });
                client.initialize().await
            };
            // Canonical discover catches MCPError around list_tools only.
            // Initialization failures, including native version mismatch, escape
            // to the host's unmapped 500 boundary without leaking their causes.
            initialize.await.map_err(|error: McpError| if modern{error}else{McpError::Unhandled})
        })
    }
}
struct McpSession {
    config: McpClientConfig,
    wire: Mutex<Option<Arc<dyn McpTransport>>>,
    session: Arc<Mutex<Option<String>>>,
}
impl McpSession {
    async fn send(&self, mut payload: Value, notification: bool) -> Result<McpHttpResponse, McpError> {
        let wire = self
            .wire
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| broker_error("MCP_NOT_CONNECTED"))?;
        let mut headers = HeaderMap::new();
        headers.insert(
            "accept",
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        headers.extend(self.config.revision.decorate(&mut payload).map_err(broker_error)?);
        if let Some(value) = self.config.credential.as_deref().filter(|v| !v.is_empty()) {
            let mut value = HeaderValue::from_str(&format!("Bearer {value}"))
                .map_err(|_| broker_error("MCP_SECRET_UNAVAILABLE"))?;
            value.set_sensitive(true);
            headers.insert("authorization", value);
        }
        if let Some(value) = self.config.origin.as_deref().filter(|v| !v.is_empty()) {
            headers.insert(
                "origin",
                HeaderValue::from_str(value.trim_end_matches('/'))
                    .map_err(|_| broker_error("MCP_ORIGIN_DENIED"))?,
            );
        }
        if let Some(value) = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref().filter(|_|!self.config.revision.modern())
        {
            let mut value =
                HeaderValue::from_str(value).map_err(|_| broker_error("MCP_SESSION_INVALID"))?;
            value.set_sensitive(true);
            headers.insert("mcp-session-id", value);
        }
        let response = wire
            .post(McpHttpRequest {
                headers,
                payload,
                notification,
                session: self.session.clone(),
            })
            .await?;
        if !(self.config.revision.modern()&&response.status==400){validate_status(response.status, notification)?;}
        self.config.revision.check_session(&response.headers).map_err(broker_error)?;
        if !self.config.revision.modern(){pin_session(&self.session, &response.headers)?;}
        validate_content_length(&response.headers, self.config.response_limit_bytes)?;
        if response.body.len() > self.config.response_limit_bytes {
            return Err(broker_error("MCP_RESPONSE_TOO_LARGE"));
        }
        Ok(response)
    }
    async fn rpc(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = uuid::Uuid::new_v4().to_string();
        let response = self
            .send(
                json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
                false,
            )
            .await?;
        let content_type=response_header(&response.headers,"content-type").unwrap_or_default();
        if self.config.revision.modern(){let parsed=decode_response_payload(&content_type,&response.body)?;if parsed["jsonrpc"]!="2.0"||parsed["id"].as_str()!=Some(id.as_str()){return Err(broker_error("MCP_RESPONSE_INVALID"))}if let Some(code)=revision::classify_error(&parsed){return Err(broker_error(code))}}
        let result=decode_jsonrpc_response(&content_type,&response.body,&id)?;
        self.config.revision.validate_result(&result).map_err(broker_error)?;
        Ok(result)
    }
}
fn pin_session(session: &Mutex<Option<String>>, headers: &HeaderMap) -> Result<(), McpError> {
    if let Some(received) = response_header(headers, "mcp-session-id") {
        if received.is_empty()
            || received.len() > 256
            || !received.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            return Err(broker_error("MCP_SESSION_INVALID"));
        }
        let mut session = session.lock().unwrap_or_else(|e| e.into_inner());
        if session.as_deref().is_some_and(|old| old != received) {
            return Err(broker_error("MCP_SESSION_CONFUSION_BLOCKED"));
        }
        *session = Some(received);
    }
    Ok(())
}
impl McpProtocolClient for McpSession {
    fn list_tools(&self) -> McpFuture<'_, Value> {
        Box::pin(async move { let mut result=self.rpc("tools/list",json!({})).await?;if self.config.revision.modern(){if let Some(tools)=result["tools"].as_array_mut(){tools.retain(|t|revision::tool_headers(&t["inputSchema"],&json!({})).is_ok());}}normalize_catalog(&result) })
    }
    fn close(&self) {
        self.wire.lock().unwrap_or_else(|e| e.into_inner()).take();
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
}

pub fn decode_response_payload(content_type: &str, body: &[u8]) -> Result<Value, McpError> {
    let content_type = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let payload: Value = match content_type.as_str() {
        "application/json" => {
            serde_json::from_slice(body.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(body))
                .map_err(|_| broker_error("MCP_RESPONSE_INVALID"))?
        }
        "text/event-stream" => {
            let text =
                std::str::from_utf8(body).map_err(|_| broker_error("MCP_RESPONSE_INVALID"))?;
            let last = text
                .split([
                    '\n', '\r', '\u{000b}', '\u{000c}', '\u{001c}', '\u{001d}', '\u{001e}',
                    '\u{0085}', '\u{2028}', '\u{2029}',
                ])
                .filter_map(|line| line.strip_prefix("data:"))
                .next_back()
                .ok_or_else(|| broker_error("MCP_RESPONSE_INVALID"))?;
            serde_json::from_str(last.trim_matches(|c: char| {
                c.is_whitespace() || ('\u{001c}'..='\u{001f}').contains(&c)
            }))
            .map_err(|_| broker_error("MCP_RESPONSE_INVALID"))?
        }
        _ => return Err(broker_error("MCP_CONTENT_TYPE_INVALID")),
    };
    if !payload.is_object() || payload.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(broker_error("MCP_RESPONSE_INVALID"));
    }
    Ok(payload)
}
pub fn decode_jsonrpc_response(
    content_type: &str,
    body: &[u8],
    request_id: &str,
) -> Result<Value, McpError> {
    let payload = decode_response_payload(content_type, body)?;
    if payload.get("id").map(python_value_to_string).as_deref() != Some(request_id) {
        return Err(broker_error("MCP_RESPONSE_ID_MISMATCH"));
    }
    if payload.get("error").is_some() {
        return Err(broker_error("MCP_REMOTE_ERROR"));
    }
    let result = payload
        .get("result")
        .filter(|v| python_truthy(v))
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !result.is_object() {
        return Err(broker_error("MCP_RESPONSE_INVALID"));
    }
    Ok(result)
}

pub fn normalize_catalog(response: &Value) -> Result<Value, McpError> {
    let raw = response
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| broker_error("MCP_CATALOG_INVALID"))?;
    let mut tools = Vec::new();
    for tool in raw.iter().take(MCP_MAX_TOOLS) {
        let Some(name) = tool
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let schema = tool
            .get("inputSchema")
            .filter(|v| python_truthy(v))
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !schema.is_object() {
            continue;
        }
        let description = tool
            .get("description")
            .filter(|v| python_truthy(v))
            .map(python_value_to_string)
            .unwrap_or_default();
        tools.push(json!({"name":name,"description":sanitize_description(&description),"inputSchema":schema,"read_only":false,"risk_level":"medium"}));
    }
    Ok(Value::Array(tools))
}

fn fold(c: char) -> char {
    match c {
        '\u{0130}' | '\u{0131}' => 'i',
        '\u{017f}' => 's',
        '\u{212a}' => 'k',
        _ => c.to_ascii_lowercase(),
    }
}
fn literal(chars: &[char], at: usize, word: &str) -> Option<usize> {
    let mut end = at;
    for c in word.chars() {
        if chars.get(end).copied().map(fold) != Some(c) {
            return None;
        }
        end += 1;
    }
    Some(end)
}
fn whitespace(chars: &[char], mut at: usize) -> usize {
    while chars.get(at).is_some_and(|c| c.is_whitespace()) {
        at += 1;
    }
    at
}
fn token_end(chars: &[char], mut at: usize) -> usize {
    while chars
        .get(at)
        .is_some_and(|c| !c.is_whitespace() && !matches!(c, ',' | ';'))
    {
        at += 1;
    }
    at
}
fn replace_matches(
    input: &str,
    matcher: impl Fn(&[char], usize) -> Option<(usize, String)>,
) -> String {
    let chars: Vec<_> = input.chars().collect();
    let mut out = String::new();
    let mut at = 0;
    while at < chars.len() {
        if let Some((end, replacement)) = matcher(&chars, at) {
            out.push_str(&replacement);
            at = end;
        } else {
            out.push(chars[at]);
            at += 1;
        }
    }
    out
}
/// Equivalent to the source's three redaction regex passes for the JSON domain.
pub fn sanitize_description(description: &str) -> String {
    let clean: String = description
        .chars()
        .map(|c| {
            if c <= '\u{001f}' || c == '\u{007f}' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let clean = replace_matches(&clean, |s, start| {
        let mut at = whitespace(s, literal(s, start, "authorization")?);
        if s.get(at) != Some(&':') {
            return None;
        }
        at = whitespace(s, at + 1);
        at = literal(s, at, "bearer")?;
        let next = whitespace(s, at);
        if next == at {
            return None;
        }
        let end = token_end(s, next);
        if end == next {
            return None;
        }
        Some((
            end,
            format!("{}[REDACTED]", s[start..next].iter().collect::<String>()),
        ))
    });
    let clean = replace_matches(&clean, |s, start| {
        if start > 0 && (s[start - 1].is_alphanumeric() || s[start - 1] == '_') {
            return None;
        }
        let mut key_end = None;
        for key in [
            "apikey", "api_key", "api-key", "token", "password", "secret",
        ] {
            if let Some(end) = literal(s, start, key) {
                key_end = Some(end);
                break;
            }
        }
        let key_end = key_end?;
        let mut at = whitespace(s, key_end);
        if !s.get(at).is_some_and(|c| matches!(c, ':' | '=')) {
            return None;
        }
        at = whitespace(s, at + 1);
        let end = token_end(s, at);
        if end == at {
            return None;
        }
        Some((
            end,
            format!(
                "{}=[REDACTED]",
                s[start..key_end].iter().collect::<String>()
            ),
        ))
    });
    let clean = replace_matches(&clean, |s, start| {
        if let Some(end) = literal(s, start, "jailbreak") {
            return Some((end, "[untrusted-instruction]".into()));
        }
        for (first, last) in [
            ("ignore", "previous"),
            ("system", "prompt"),
            ("developer", "message"),
        ] {
            if let Some(end) = literal(s, start, first) {
                let mut at = whitespace(s, end);
                if at == end {
                    continue;
                }
                if first == "ignore"
                    && let Some(all) = literal(s, at, "all")
                {
                    let after = whitespace(s, all);
                    if after > all {
                        at = after;
                    }
                }
                if let Some(end) = literal(s, at, last) {
                    return Some((end, "[untrusted-instruction]".into()));
                }
            }
        }
        None
    });
    clean
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(500)
        .collect()
}

// Definitions remain pending until the root integrates this unmounted module.

#[cfg(test)]
#[path="mcp_http/session_tests.rs"]
mod revision_session_tests;
