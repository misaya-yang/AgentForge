//! Opt-in HTTP collaborator for canonical `src/services/llm/provider_service.py`.
//!
//! Construction is inert. Each polled send creates a fresh client, like the source's
//! per-probe AsyncClient, without wiring routes, workers or provider discovery.
//! Redirects and automatic retries are disabled. The response body is fully drained
//! (and discarded) before latency is measured, including non-success responses.
//! All failures are mapped to fixed ServiceError variants, never upstream text,
//! URLs, request bodies, headers, credentials or reqwest error strings.
//!
//! Timeout boundary: httpx's scalar timeout is independent connect/read/write/pool
//! timeouts, NOT an overall deadline. We use reqwest connect_timeout/read_timeout
//! and intentionally do not set its total timeout. Read timeout resets on progress,
//! including response body reads. Reqwest does not expose an independent write or
//! pool-acquisition timeout; this adapter does not claim those semantics. Its
//! header-wait timer starts during send and can cover upload time as well. A fresh
//! client has no shared saturated connection pool. No caller-wide deadline is added.
//!
//! Other transport gaps: TLS roots, proxy environment handling, default headers,
//! URL normalization, HTTP version negotiation and JSON number formatting follow
//! reqwest, not httpx. Current workspace features do not include decompression;
//! compressed responses are drained as raw bytes. No response text is exposed.
//! Cancellation drops the in-flight request/response and client without spawning
//! an application task; DNS/transport internals remain owned by reqwest's runtime.

use crate::provider_service::{
    ProbeRequest, ProbeResponse, ProviderHttp, ServiceError, ServiceFuture,
};
use reqwest::{
    Client, Method, Url,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use std::{
    fmt,
    time::{Duration, Instant},
};

/// Explicitly injected transport. Creating it never reads environment or opens I/O.
#[derive(Clone, Copy, Default)]
pub struct ReqwestProviderHttp;

impl ReqwestProviderHttp {
    pub const fn new() -> Self {
        Self
    }
}

impl fmt::Debug for ReqwestProviderHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ReqwestProviderHttp(<internal>)")
    }
}

/// Check timeout first: a connect timeout can satisfy both classifications.
fn transport_error(error: reqwest::Error) -> ServiceError {
    if error.is_timeout() {
        ServiceError::Timeout
    } else if error.is_connect() {
        ServiceError::Connect
    } else {
        ServiceError::Http
    }
}

impl ProviderHttp for ReqwestProviderHttp {
    fn send<'a>(&'a self, request: ProbeRequest) -> ServiceFuture<'a, ProbeResponse> {
        Box::pin(async move {
            // Validation precedes client construction, and never formats inputs.
            let method =
                Method::from_bytes(request.method.as_bytes()).map_err(|_| ServiceError::Http)?;
            let url = Url::parse(&request.url).map_err(|_| ServiceError::Http)?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return Err(ServiceError::Http);
            }
            let mut headers = HeaderMap::new();
            for (name, value) in request.headers {
                let name =
                    HeaderName::from_bytes(name.as_bytes()).map_err(|_| ServiceError::Http)?;
                let mut value = HeaderValue::from_str(&value).map_err(|_| ServiceError::Http)?;
                // Treat even custom API-key headers as sensitive to Debug output.
                value.set_sensitive(true);
                headers.append(name, value);
            }
            let timeout = Duration::from_secs(request.timeout_seconds);
            let client = Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .connect_timeout(timeout)
                .read_timeout(timeout)
                .connection_verbose(false)
                .build()
                .map_err(transport_error)?;
            let mut builder = client.request(method, url).headers(headers);
            if let Some(body) = request.body {
                builder = builder.json(&body);
            }
            let request = builder.build().map_err(transport_error)?;
            // httpx starts elapsed measurement around transport send, after request
            // preparation, and closes its bound stream after reading the body.
            let started = Instant::now();
            let mut response = client.execute(request).await.map_err(transport_error)?;
            let status = response.status().as_u16();
            while response.chunk().await.map_err(transport_error)?.is_some() {}
            let elapsed_milliseconds =
                u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            Ok(ProbeResponse {
                status,
                elapsed_milliseconds,
            })
        })
    }
}
