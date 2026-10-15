//! Opt-in GET collaborator for canonical `src/services/llm/model_catalog_sync.py`.
//!
//! Construction and unpolled futures are inert. Every polled, valid request owns a
//! fresh reqwest client, matching the source's default per-call AsyncClient. The
//! orchestration supplies GET with either a Google `key` query parameter or an
//! OpenAI-compatible Bearer header, and 10 seconds. No credentials, environment
//! or services are inspected here.
//! Client creation may consult the host's proxy/TLS configuration, only when polled.
//! Redirects and retries are disabled; status codes are returned without raising.
//! All response bytes are consumed before returning, including non-200 responses.
//! JSON decode failure stays inside `CatalogResponse::json`, so the orchestration
//! can use its existing trusted-catalog fallback. Transport failures discard every
//! upstream error string; this module never logs URLs, keys, bodies or errors.
//!
//! The query uses httpx 0.28.1's QueryParams/urllib.parse.urlencode semantics, not
//! reqwest `.query()`: supplied params replace the URL query, spaces become `+`,
//! `~` remains literal, `*` is escaped, and duplicate keys are grouped in first-key
//! order. No authorization/API-key headers are synthesized by this adapter.
//!
//! Timeout boundary: httpx's scalar timeout independently configures connect,
//! read, write and pool waits. Reqwest connect/read timeouts are set independently;
//! no total timeout is added. Read timeout resets on body progress. Reqwest cannot
//! independently configure write/pool timeouts, and its response-header timer can
//! include connect/upload time. A fresh client has no shared saturated pool. The
//! source's 10 seconds comes from the frozen request, not a new overall deadline.
//!
//! Host boundaries, explicitly not full wire parity: TLS roots, proxy environment,
//! URL/path normalization, default headers and HTTP negotiation follow reqwest.
//! The existing dependency features do not enable compressed-body decoding. JSON
//! is decoded from raw UTF-8 bytes with serde_json: unlike Python json.loads it
//! rejects NaN/Infinity, UTF-8 BOMs, UTF-16/32, and unpaired surrogate escapes; its
//! default nesting limit differs. Integers beyond i64/u64 can round to f64 (or fail
//! when out of range), whereas Python retains arbitrary-size integers. Overflowed
//! floats are rejected rather than becoming Python infinity. These limitations
//! can change discovery metadata or trigger fallback; serde Value cannot represent
//! Python's non-finite numbers/arbitrary integers. The HTTP Content-Type/charset
//! does not control JSON decoding, as in the source's response.json(). Duplicate
//! object names retain the last value. No raw response/error text is attached to
//! failures.
//!
//! Supplied clients are intentionally unsupported: an arbitrary reqwest Client's
//! connect/read settings cannot be overridden per request as the source does with
//! timeout=10.0, nor can its redirect/retry/default-auth policy be verified. The
//! CatalogHttp trait still permits a host-owned collaborator. On completion or
//! cancellation this adapter drops its response/client; unlike Python's awaited
//! AsyncClient.aclose(), Rust Drop does not await transport shutdown. Reqwest owns
//! any runtime/DNS internals. No application task, pool or client is retained here.

use crate::model_catalog_sync::{
    CatalogHttp, CatalogHttpError, CatalogHttpFuture, CatalogRequest, CatalogResponse,
};
use reqwest::{
    Client, Url,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use std::{fmt, time::Duration};

/// Inert, explicitly injected transport. Not installed into any live service.
#[derive(Clone, Copy, Default)]
pub struct ReqwestCatalogHttp;

impl ReqwestCatalogHttp {
    pub const fn new() -> Self {
        Self
    }
}

impl fmt::Debug for ReqwestCatalogHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ReqwestCatalogHttp(<internal>)")
    }
}

fn transport_error(error: reqwest::Error) -> CatalogHttpError {
    if error.is_timeout() {
        CatalogHttpError::Timeout
    } else {
        CatalogHttpError::Transport
    }
}

/// Python quote_plus(..., safe="") over UTF-8, including its always-safe `~`.
fn append_query_component(output: &mut String, value: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                output.push(char::from(byte));
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

fn query_string(params: &[(String, String)]) -> String {
    // httpx QueryParams groups duplicate keys while preserving first-key order.
    // The canonical source only provides one key; retain its sequence semantics
    // for host-created requests without introducing an unordered map.
    let mut query = String::new();
    for (index, (name, _)) in params.iter().enumerate() {
        if params[..index].iter().any(|(previous, _)| previous == name) {
            continue;
        }
        for (_, value) in params.iter().filter(|(key, _)| key == name) {
            if !query.is_empty() {
                query.push('&');
            }
            append_query_component(&mut query, name);
            query.push('=');
            append_query_component(&mut query, value);
        }
    }
    query
}

fn prepare(request: &CatalogRequest) -> Result<(Url, HeaderMap), CatalogHttpError> {
    // The source collaborator is GET-only. Validate before constructing a client
    // (which may inspect host configuration) and never format rejected inputs.
    if request.method != "GET" {
        return Err(CatalogHttpError::Transport);
    }
    let mut url = Url::parse(&request.url).map_err(|_| CatalogHttpError::Transport)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(CatalogHttpError::Transport);
    }
    let query = query_string(&request.params);
    url.set_query((!request.params.is_empty()).then_some(query.as_str()));
    let mut headers = HeaderMap::new();
    for (name, value) in &request.headers {
        let name =
            HeaderName::from_bytes(name.as_bytes()).map_err(|_| CatalogHttpError::Transport)?;
        let mut value = HeaderValue::from_str(value).map_err(|_| CatalogHttpError::Transport)?;
        value.set_sensitive(true);
        headers.append(name, value);
    }
    Ok((url, headers))
}

impl CatalogHttp for ReqwestCatalogHttp {
    fn get(&self, request: CatalogRequest) -> CatalogHttpFuture<'_> {
        Box::pin(async move {
            let (url, headers) = prepare(&request)?;
            let timeout = Duration::from_secs(request.timeout_seconds);
            let client = Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .connect_timeout(timeout)
                .read_timeout(timeout)
                .connection_verbose(false)
                .build()
                .map_err(transport_error)?;
            let response = client
                .get(url)
                .headers(headers)
                .send()
                .await
                .map_err(transport_error)?;
            let status_code = response.status().as_u16();
            // Like httpx's non-streaming get(), consume even an error-status body.
            // A body transport failure is distinct from a fully-read invalid JSON.
            let bytes = response.bytes().await.map_err(transport_error)?;
            let json = serde_json::from_slice(&bytes).map_err(|_| CatalogHttpError::InvalidJson);
            Ok(CatalogResponse { status_code, json })
        })
    }
}

