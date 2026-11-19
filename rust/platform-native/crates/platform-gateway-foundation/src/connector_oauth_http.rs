//! Production HTTP adapters for canonical `src/api/v1/connectors.py`.
//!
//! OAuth forms use `safe_fetch.py::safe_form_post`: HTTPS, no userinfo/nonempty
//! fragment, every DNS answer checked, first returned address pinned, original
//! hostname retained for TLS identity/SNI and Host, and redirects never followed.
//! A fresh reqwest client has no automatic retries. Responses include status and
//! the complete body, including error/redirect bodies; JSON parsing belongs to
//! the caller. Like source, no response-size cap is silently introduced.
//!
//! Source predicate nuance: 100.64.0.0/10 passes Python's unsafe-IP predicate
//! despite not being globally reachable, and IPv4-mapped IPv6 is checked using
//! its embedded IPv4. We preserve those two differences from MCP's stricter
//! address predicate. Remaining address tables reuse the existing Python 3.12.14
//! native predicate. Every answer must pass; the first, not sorted minimum, is
//! selected, matching safe_form_post rather than the native MCP client.
//!
//! OAuth explicitly disables environment proxies so a proxy cannot bypass the
//! validated pin. This agrees with locked httpx 0.28.1, whose AsyncClient disables
//! environment-proxy mounts when passed a custom transport (_client.py:1303).
//! Provider GET retains native proxy configuration and does NOT apply OAuth DNS
//! pinning, like source's plain AsyncClient. Explicit network-policy difference:
//! provider URLs require HTTPS without userinfo, a defensive boundary
//! beyond the generic source client; all canonical provider endpoints meet it.
//! Source provider IDs can inject path/query components; this adapter does not
//! invent segment validation. URL normalization, default headers, HTTP negotiation
//! and TLS trust roots are reqwest's. No compressed-body support is added beyond
//! enabled dependency features. Source Host omits even a nondefault port; the
//! OAuth adapter preserves that override while keeping the URL port for connect.
//!
//! Source scalar timeouts are 10 seconds for forms and 5 seconds for provider
//! GET. Independent reqwest connect/read timers preserve the closest available
//! boundary, with no total deadline; independent write/pool timers are unavailable.
//! A fresh client avoids shared-pool contention. DNS is outside those timers, as
//! in source; cancelling cannot stop an OS lookup already in progress. Rust Drop
//! is not source's awaited aclose. Credentials, URLs, bodies and upstream error
//! text are never logged or exposed by Debug/errors. Construction and unpolled
//! futures do no I/O. Host wiring remains explicitly opt-in.

use crate::{
    connector_user_types::{
        ConnectorGetRequest, ConnectorHttpError, ConnectorHttpFuture, ConnectorHttpResponse,
        ConnectorProviderHttp, OAuthFormHttp, OAuthFormRequest,
    },
    mcp_http::mcp_public_address,
};
use reqwest::{
    Client, Method, Url,
    header::{CONTENT_TYPE, HOST, HeaderMap, HeaderName, HeaderValue},
};
use std::{
    fmt,
    future::Future,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

pub type ConnectorDnsFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<IpAddr>, ConnectorHttpError>> + Send + 'a>>;

/// Trusted injected resolver; the adapter, not the resolver, checks every answer.
pub trait ConnectorDnsResolver: Send + Sync {
    fn resolve<'a>(&'a self, hostname: &'a str, port: u16) -> ConnectorDnsFuture<'a>;
}

#[derive(Clone, Copy, Default, Debug)]
pub struct NativeConnectorDnsResolver;

impl ConnectorDnsResolver for NativeConnectorDnsResolver {
    fn resolve<'a>(&'a self, hostname: &'a str, port: u16) -> ConnectorDnsFuture<'a> {
        Box::pin(async move {
            let hostname = hostname.to_owned();
            tokio::task::spawn_blocking(move || {
                (hostname.as_str(), port)
                    .to_socket_addrs()
                    .map(|addresses| addresses.map(|address| address.ip()).collect())
            })
            .await
            .map_err(|_| ConnectorHttpError::RejectedDestination)?
            .map_err(|_| ConnectorHttpError::RejectedDestination)
        })
    }
}

/// Source's explicit unsafe predicate, which is not Python `is_global`.
pub fn oauth_form_safe_address(address: IpAddr) -> bool {
    let address = match address {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(address),
        _ => address,
    };
    if let IpAddr::V4(ip) = address
        && u32::from(ip) >> 22 == 0x64400000u32 >> 22
    {
        return true; // Source permits shared address space, 100.64.0.0/10.
    }
    mcp_public_address(address)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectorRequestPolicy {
    /// Original URL's host remains the TLS/SNI identity; this is the only socket.
    PinnedOAuth { address: IpAddr, port: u16 },
    /// Canonical provider calls use ordinary DNS and configured proxies.
    ProviderGet,
}

/// Fields are private so only validated adapter preparation can create a request.
/// Getters are for trusted in-process transports/fixtures, never serialization.
pub struct PreparedConnectorRequest {
    method: Method,
    url: Url,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
    policy: ConnectorRequestPolicy,
    timeout: Duration,
}

impl PreparedConnectorRequest {
    pub fn method(&self) -> &Method {
        &self.method
    }
    pub fn url(&self) -> &Url {
        &self.url
    }
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }
    pub fn body(&self) -> Option<&[u8]> {
        self.body.as_deref()
    }
    pub fn policy(&self) -> ConnectorRequestPolicy {
        self.policy
    }
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
    pub const fn follows_redirects(&self) -> bool {
        false
    }
    pub const fn automatically_retries(&self) -> bool {
        false
    }
}

impl fmt::Debug for PreparedConnectorRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PreparedConnectorRequest(<redacted>)")
    }
}

/// Inject only a trusted transport. Production uses ReqwestConnectorTransport.
/// Deterministic fixtures replace this seam after real validation/preparation.
pub trait ConnectorRequestTransport: Send + Sync {
    fn execute(&self, request: PreparedConnectorRequest) -> ConnectorHttpFuture<'_>;
}

#[derive(Clone, Copy, Default, Debug)]
pub struct ReqwestConnectorTransport;

impl ConnectorRequestTransport for ReqwestConnectorTransport {
    fn execute(&self, request: PreparedConnectorRequest) -> ConnectorHttpFuture<'_> {
        Box::pin(async move {
            let mut builder = Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .connect_timeout(request.timeout)
                .read_timeout(request.timeout)
                .connection_verbose(false);
            if let ConnectorRequestPolicy::PinnedOAuth { address, port } = request.policy {
                let hostname = request
                    .url
                    .host_str()
                    .ok_or(ConnectorHttpError::RejectedDestination)?
                    .trim_matches(['[', ']']);
                // The URL, and therefore TLS identity, is never rewritten to an IP.
                // Literal IP URLs use that same checked IP and need no second DNS.
                builder = builder
                    .no_proxy()
                    .resolve(hostname, SocketAddr::new(address, port));
            }
            let client = builder.build().map_err(|_| ConnectorHttpError::Transport)?;
            let mut outgoing = client
                .request(request.method, request.url)
                .headers(request.headers);
            if let Some(body) = request.body {
                outgoing = outgoing.body(body);
            }
            let response = outgoing
                .send()
                .await
                .map_err(|_| ConnectorHttpError::Transport)?;
            let status = response.status().as_u16();
            let body = response
                .bytes()
                .await
                .map_err(|_| ConnectorHttpError::Transport)?
                .to_vec();
            Ok(ConnectorHttpResponse { status, body })
        })
    }
}

#[derive(Clone)]
pub struct ReqwestOAuthFormHttp {
    resolver: Arc<dyn ConnectorDnsResolver>,
    transport: Arc<dyn ConnectorRequestTransport>,
}

impl ReqwestOAuthFormHttp {
    pub fn new() -> Self {
        Self::with_collaborators(
            Arc::new(NativeConnectorDnsResolver),
            Arc::new(ReqwestConnectorTransport),
        )
    }

    pub fn with_collaborators(
        resolver: Arc<dyn ConnectorDnsResolver>,
        transport: Arc<dyn ConnectorRequestTransport>,
    ) -> Self {
        Self {
            resolver,
            transport,
        }
    }
}

impl Default for ReqwestOAuthFormHttp {
    fn default() -> Self {
        Self::new()
    }
}
impl fmt::Debug for ReqwestOAuthFormHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ReqwestOAuthFormHttp(<internal>)")
    }
}

#[derive(Clone)]
pub struct ReqwestConnectorProviderHttp {
    transport: Arc<dyn ConnectorRequestTransport>,
}

impl ReqwestConnectorProviderHttp {
    pub fn new() -> Self {
        Self::with_transport(Arc::new(ReqwestConnectorTransport))
    }
    pub fn with_transport(transport: Arc<dyn ConnectorRequestTransport>) -> Self {
        Self { transport }
    }
}
impl Default for ReqwestConnectorProviderHttp {
    fn default() -> Self {
        Self::new()
    }
}
impl fmt::Debug for ReqwestConnectorProviderHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ReqwestConnectorProviderHttp(<internal>)")
    }
}

fn parse_url(raw: &str, form: bool) -> Result<Url, ConnectorHttpError> {
    let url = Url::parse(raw).map_err(|_| ConnectorHttpError::RejectedDestination)?;
    // username() alone misses empty userinfo (https://@host), which source rejects.
    let has_userinfo = raw.split_once("://").is_some_and(|(_, suffix)| {
        suffix
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("")
            .contains('@')
    });
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || has_userinfo
        || (form && url.fragment().is_some_and(|fragment| !fragment.is_empty()))
    {
        return Err(ConnectorHttpError::RejectedDestination);
    }
    Ok(url)
}

fn request_headers(headers: Vec<(String, String)>) -> Result<HeaderMap, ConnectorHttpError> {
    let mut result = HeaderMap::new();
    for (name, value) in headers {
        // httpx encodes supplied str header names/values as ASCII before network
        // I/O. UnicodeEncodeError is outside safe_form_post's HTTPError catch.
        // HeaderValue accepts non-ASCII bytes, so enforce the source boundary.
        if !name.is_ascii() || !value.is_ascii() {
            return Err(ConnectorHttpError::SourceUnhandled);
        }
        let name =
            HeaderName::from_bytes(name.as_bytes()).map_err(|_| ConnectorHttpError::Transport)?;
        let mut value = HeaderValue::from_str(&value).map_err(|_| ConnectorHttpError::Transport)?;
        value.set_sensitive(true);
        result.append(name, value);
    }
    Ok(result)
}

// urllib.parse.quote_plus over UTF-8: ~ stays literal, * does not, spaces use +.
fn component(output: &mut String, value: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                output.push(char::from(byte))
            }
            b' ' => output.push('+'),
            _ => {
                output.push('%');
                output.push(char::from(HEX[usize::from(byte >> 4)]));
                output.push(char::from(HEX[usize::from(byte & 15)]));
            }
        }
    }
}

fn encode_fields(fields: &[(String, String)], group_keys: bool) -> String {
    let mut result = String::new();
    let mut append = |name: &str, value: &str| {
        if !result.is_empty() {
            result.push('&');
        }
        component(&mut result, name);
        result.push('=');
        component(&mut result, value);
    };
    for (index, (name, value)) in fields.iter().enumerate() {
        if !group_keys {
            append(name, value);
        } else if !fields[..index].iter().any(|(previous, _)| previous == name) {
            // httpx QueryParams groups repeated names in first-name order.
            for (_, value) in fields.iter().filter(|(key, _)| key == name) {
                append(name, value);
            }
        }
    }
    result
}

impl OAuthFormHttp for ReqwestOAuthFormHttp {
    fn post(&self, request: OAuthFormRequest) -> ConnectorHttpFuture<'_> {
        Box::pin(async move {
            let url = parse_url(&request.url, true)?;
            let hostname = url
                .host_str()
                .ok_or(ConnectorHttpError::RejectedDestination)?
                .trim_matches(['[', ']'])
                .to_owned();
            let port = url
                .port_or_known_default()
                .ok_or(ConnectorHttpError::RejectedDestination)?;
            let addresses = self.resolver.resolve(&hostname, port).await?;
            if addresses.is_empty()
                || addresses
                    .iter()
                    .any(|address| !oauth_form_safe_address(*address))
            {
                return Err(ConnectorHttpError::RejectedDestination);
            }
            let address = addresses[0];
            if hostname
                .parse::<IpAddr>()
                .is_ok_and(|literal| literal != address)
            {
                return Err(ConnectorHttpError::RejectedDestination);
            }
            let mut headers = request_headers(request.headers)?;
            // Source pinned transport unconditionally replaces caller-supplied Host.
            let host =
                HeaderValue::from_str(&hostname).map_err(|_| ConnectorHttpError::Transport)?;
            headers.insert(HOST, host);
            headers
                .entry(CONTENT_TYPE)
                .or_insert(HeaderValue::from_static(
                    "application/x-www-form-urlencoded",
                ));
            let body = encode_fields(&request.fields, false).into_bytes();
            self.transport
                .execute(PreparedConnectorRequest {
                    method: Method::POST,
                    url,
                    headers,
                    body: Some(body),
                    policy: ConnectorRequestPolicy::PinnedOAuth { address, port },
                    timeout: Duration::from_secs(10),
                })
                .await
        })
    }
}

impl ConnectorProviderHttp for ReqwestConnectorProviderHttp {
    fn get(&self, request: ConnectorGetRequest) -> ConnectorHttpFuture<'_> {
        Box::pin(async move {
            let mut url = parse_url(&request.url, false)?;
            // Canonical no-param metadata GET has no query to replace. An empty
            // query vector means absent params, preserving an existing URL query.
            if !request.query.is_empty() {
                url.set_query(Some(&encode_fields(&request.query, true)));
            }
            let headers = request_headers(request.headers)?;
            self.transport
                .execute(PreparedConnectorRequest {
                    method: Method::GET,
                    url,
                    headers,
                    body: None,
                    policy: ConnectorRequestPolicy::ProviderGet,
                    timeout: Duration::from_secs(5),
                })
                .await
        })
    }
}
