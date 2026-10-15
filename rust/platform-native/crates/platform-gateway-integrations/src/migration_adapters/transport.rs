//! Caller-owned HTTP transport. Stream ownership includes the connection permit;
//! dropping/closing a producer drops its response and cancels all pending reads.
use super::*;
use reqwest::{
    Client, Response, Url,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use std::{sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub struct HttpTransport {
    pub client: Client,
    service: ServiceHandle,
    captured_config: Value,
    base: String,
    slots: Option<Arc<Semaphore>>,
    pool_timeout: Option<Duration>,
}
pub struct HttpResponse {
    pub response: Response,
    pub permit: Option<OwnedSemaphorePermit>,
}
impl HttpTransport {
    pub fn new(service: ServiceHandle) -> Result<Self, RegistryError> {
        if service.snapshot().connector_type != TransportType::Http {
            return Err(RegistryError::AdapterConstructionFailed);
        }
        let config = service.connector().snapshot();
        let bad = || RegistryError::AdapterConstructionFailed;
        let duration = |key: &str, default: f64| -> Result<Option<Duration>, RegistryError> {
            match config.get(key) {
                Some(Value::Null) => Ok(None),
                value => Duration::try_from_secs_f64(
                    value
                        .map(|v| v.as_f64().ok_or_else(bad))
                        .transpose()?
                        .unwrap_or(default),
                )
                .map(Some)
                .map_err(|_| bad()),
            }
        };
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never());
        // Reqwest's ordinary negotiation enables HTTP/2 through TLS ALPN when
        // the workspace enables its http2 feature. Do not force prior knowledge:
        // the source httpx client continues to accept HTTP/1 servers.
        if !config.get("http2").is_some_and(python_truthy) {
            builder = builder.http1_only();
        }
        if let Some(t) = duration("connect_timeout", 3.)? {
            builder = builder.connect_timeout(t);
        }
        if let Some(t) = duration("stream_timeout", 300.)? {
            builder = builder.read_timeout(t);
        }
        builder = builder.pool_idle_timeout(duration("keepalive_expiry", 120.)?);
        if let Some(n) = config.get("keepalive_connections").filter(|v| !v.is_null()) {
            builder = builder.pool_max_idle_per_host(n.as_u64().ok_or_else(bad)? as usize);
        } else if config.get("keepalive_connections").is_none() {
            builder = builder.pool_max_idle_per_host(50);
        }
        let mut headers = HeaderMap::new();
        if let Some(h) = config.get("headers").filter(|v| python_truthy(v)) {
            for (k, v) in h.as_object().ok_or_else(bad)? {
                let mut v =
                    HeaderValue::from_str(v.as_str().ok_or_else(bad)?).map_err(|_| bad())?;
                v.set_sensitive(true);
                headers.insert(HeaderName::from_bytes(k.as_bytes()).map_err(|_| bad())?, v);
            }
        }
        let slots = match config.get("max_connections") {
            Some(Value::Null) => None,
            n => {
                let n = n
                    .map(|v| v.as_u64().ok_or_else(bad))
                    .transpose()?
                    .unwrap_or(200);
                let n = usize::try_from(n).map_err(|_| bad())?;
                if n > Semaphore::MAX_PERMITS {
                    return Err(bad());
                }
                Some(Arc::new(Semaphore::new(n)))
            }
        };
        Ok(Self {
            client: builder
                .default_headers(headers)
                .build()
                .map_err(|_| bad())?,
            base: config
                .get("base_url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim_end_matches('/')
                .into(),
            pool_timeout: duration("pool_timeout", 10.)?,
            slots,
            captured_config: config.clone(),
            service,
        })
    }
    /// Construct once per LangGraph adapter, then retain beside the ordinary
    /// client. Reqwest exposes connect/read timeouts on clients, not requests.
    /// Both clients share the same connection admission semaphore, so streaming
    /// cannot bypass max_connections. Idle connection pools remain separate.
    /// The source's independent 30s write timeout has no reqwest equivalent;
    /// using a 30s request deadline would incorrectly kill valid long streams.
    pub fn langgraph_stream_variant(&self) -> Result<Self, RegistryError> {
        let mut snapshot = self.service.snapshot();
        let mut config = self
            .captured_config
            .as_object()
            .cloned()
            .unwrap_or_default();
        config.insert("connect_timeout".into(), json!(5.0));
        config.insert("stream_timeout".into(), json!(300.0));
        config.insert("pool_timeout".into(), json!(10.0));
        snapshot.connector_config = Value::Object(config);
        let mut stream = Self::new(ServiceHandle::new(snapshot))?;
        stream.service = self.service.clone();
        stream.slots = self.slots.clone();
        Ok(stream)
    }
    pub fn url(&self, endpoint: &str) -> Result<Url, AdapterError> {
        if endpoint.starts_with("//") {
            return Err(validation("unsupported_scheme_relative_endpoint"));
        }
        let url = if endpoint.contains("://") {
            Url::parse(endpoint)
        } else {
            Url::parse(&format!("{}/", self.base))
                .and_then(|base| base.join(endpoint.trim_start_matches('/')))
        }
        .map_err(|_| validation("invalid_http_endpoint"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(validation("unsupported_http_endpoint_scheme"));
        }
        Ok(url)
    }
    async fn acquire(&self) -> Result<Option<OwnedSemaphorePermit>, AdapterError> {
        let Some(slots) = &self.slots else {
            return Ok(None);
        };
        let future = slots.clone().acquire_owned();
        let permit = if let Some(timeout) = self.pool_timeout {
            tokio::time::timeout(timeout, future)
                .await
                .map_err(|_| AdapterFailure::PoolTimeout)?
        } else {
            future.await
        };
        Ok(Some(permit.map_err(|_| AdapterFailure::Other)?))
    }
    pub async fn send(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<HttpResponse, AdapterError> {
        let request = builder.build().map_err(|e| transport_error(&e, false))?;
        let permit = self.acquire().await?;
        let response = self
            .client
            .execute(request)
            .await
            .map_err(|e| transport_error(&e, false))?;
        Ok(HttpResponse { response, permit })
    }
    pub async fn post_json(
        &self,
        endpoint: &str,
        body: &Value,
        headers: HeaderMap,
    ) -> Result<HttpResponse, AdapterError> {
        self.send(
            self.client
                .post(self.url(endpoint)?)
                .headers(headers)
                .json(body),
        )
        .await
    }
    pub async fn health(&self) -> bool {
        let config = self.service.connector().snapshot();
        let endpoint = config
            .get("health_endpoint")
            .and_then(Value::as_str)
            .unwrap_or("/health");
        let Ok(url) = self.url(endpoint) else {
            return false;
        };
        let Ok(response) = self.send(self.client.get(url)).await else {
            return false;
        };
        response.response.status().as_u16() < 500 && response.response.bytes().await.is_ok()
    }
}
impl HttpResponse {
    pub fn check_status(&self) -> Result<(), AdapterError> {
        let status = self.response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(AdapterFailure::HttpStatus(status).into());
        }
        Ok(())
    }
    pub async fn body(self) -> Result<(bool, Vec<u8>), AdapterError> {
        let status = self.response.status().as_u16();
        let json = self
            .response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("application/json"));
        let bytes = self
            .response
            .bytes()
            .await
            .map_err(|e| transport_error(&e, true))?;
        if !(200..300).contains(&status) {
            return Err(AdapterFailure::HttpStatus(status).into());
        }
        Ok((json, bytes.to_vec()))
    }
    pub async fn json(self) -> Result<Value, AdapterError> {
        let (_, bytes) = self.body().await?;
        serde_json::from_slice(&bytes).map_err(|_| validation("provider_response_json_invalid"))
    }
}
pub fn transport_error(e: &reqwest::Error, reading: bool) -> AdapterError {
    (if e.is_connect() {
        if e.is_timeout() {
            AdapterFailure::Other
        } else {
            AdapterFailure::ConnectError
        }
    } else if e.is_timeout() {
        if reading {
            AdapterFailure::ReadTimeout
        } else {
            AdapterFailure::WriteTimeout
        }
    } else if reading {
        AdapterFailure::ReadError
    } else {
        AdapterFailure::WriteError
    })
    .into()
}
/// SDK already caps framed stream bytes at 4 MiB. Apply that ceiling before
/// framing, and use the same explicit defensive default for OpenAI-compatible
/// streams. Dify narrows it to its existing event budget plus the SSE prefix.
const MAX_STREAM_LINE_BYTES: usize = 4 * 1024 * 1024;

/// Buffered line framing keeps separate consume and scan cursors. A yielded line
/// does not shift the remaining chunk; an incomplete line is not rescanned after
/// each upstream fragment. Compact only when appending more staged bytes.
struct LineBuffer {
    bytes: Vec<u8>,
    consumed: usize,
    scanned: usize,
    max_line_bytes: usize,
}
impl Default for LineBuffer {
    fn default() -> Self {
        Self::with_limit(MAX_STREAM_LINE_BYTES)
    }
}
impl LineBuffer {
    fn with_limit(max_line_bytes: usize) -> Self {
        // Only protocol-owned constants (or smaller test budgets) are admitted.
        assert!((1..=MAX_STREAM_LINE_BYTES).contains(&max_line_bytes));
        Self { bytes: Vec::new(), consumed: 0, scanned: 0, max_line_bytes }
    }
    fn clear(&mut self) {
        // A terminal producer must not retain its largest allocation.
        self.bytes = Vec::new();
        self.consumed = 0;
        self.scanned = 0;
    }
    fn compact(&mut self) {
        if self.consumed > 0 {
            self.bytes.drain(..self.consumed);
            self.scanned -= self.consumed;
            self.consumed = 0;
        }
    }
    /// Copy only bounded staging bytes; the caller retains the unconsumed HTTP
    /// chunk without copying it. Two extra bytes cover CRLF and CR look-ahead.
    fn push(&mut self, bytes: &[u8]) -> usize {
        self.compact();
        let count = bytes.len().min(self.max_line_bytes + 2 - self.bytes.len());
        if self.bytes.len() + count > self.bytes.capacity() {
            // Keep geometric growth without requesting Vec's next doubled
            // capacity beyond the protocol ceiling. Allocator overhead is separate.
            let capacity = self.bytes.capacity().saturating_mul(2)
                .max(self.bytes.len() + count).min(self.max_line_bytes + 2);
            self.bytes.reserve_exact(capacity - self.bytes.len());
        }
        self.bytes.extend_from_slice(&bytes[..count]);
        count
    }
    fn next(&mut self, eof: bool) -> Result<Option<String>, &'static str> {
        if let Some(offset) = self.bytes[self.scanned..]
            .iter()
            .position(|b| *b == b'\n' || *b == b'\r')
        {
            let pos = self.scanned + offset;
            if pos - self.consumed > self.max_line_bytes {
                return Err("provider_stream_line_too_large");
            }
            if self.bytes[pos] == b'\r' && pos + 1 == self.bytes.len() && !eof {
                // Revisit just this CR when its possible LF arrives, not the line.
                self.scanned = pos;
                return Ok(None);
            }
            let line = String::from_utf8(self.bytes[self.consumed..pos].to_vec())
                .map_err(|_| "provider_stream_utf8_invalid")?;
            self.consumed = pos
                + if self.bytes[pos] == b'\r' && self.bytes.get(pos + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
            self.scanned = self.consumed;
            return Ok(Some(line));
        }
        self.scanned = self.bytes.len();
        if self.bytes.len() - self.consumed > self.max_line_bytes {
            return Err("provider_stream_line_too_large");
        }
        if eof && self.consumed < self.bytes.len() {
            self.compact();
            self.scanned = 0;
            return String::from_utf8(std::mem::take(&mut self.bytes))
                .map(Some)
                .map_err(|_| "provider_stream_utf8_invalid");
        }
        Ok(None)
    }
}

/// Incremental UTF-8 line framing survives split multibyte characters and CRLF.
pub struct Lines {
    response: Option<HttpResponse>,
    buffer: LineBuffer,
    pending_chunk: Option<axum::body::Bytes>,
    eof: bool,
}
impl Lines {
    pub fn new(response: HttpResponse) -> Result<Self, AdapterError> {
        Self::with_max_line_bytes(response, MAX_STREAM_LINE_BYTES)
    }
    pub(super) fn with_max_line_bytes(
        response: HttpResponse,
        max_line_bytes: usize,
    ) -> Result<Self, AdapterError> {
        response.check_status()?;
        Ok(Self {
            response: Some(response),
            buffer: LineBuffer::with_limit(max_line_bytes),
            pending_chunk: None,
            eof: false,
        })
    }
    pub fn close(&mut self) {
        self.response = None;
        self.pending_chunk = None;
        self.buffer.clear();
        self.eof = true;
    }
    pub async fn next(&mut self) -> Result<Option<String>, AdapterError> {
        loop {
            match self.buffer.next(self.eof) {
                Ok(Some(line)) => return Ok(Some(line)),
                Ok(None) => {}
                Err(code) => {
                    // Release the raw chunk, HTTP body, permit and staging
                    // allocation before reporting the terminal validation error.
                    self.close();
                    return Err(validation(code));
                }
            }
            if let Some(bytes) = self.pending_chunk.take() {
                let consumed = self.buffer.push(&bytes);
                if consumed < bytes.len() {
                    self.pending_chunk = Some(bytes.slice(consumed..));
                }
                continue;
            }
            if self.eof {
                return Ok(None);
            }
            match self
                .response
                .as_mut()
                .expect("open response")
                .response
                .chunk()
                .await
                .map_err(|e| transport_error(&e, true))?
            {
                Some(bytes) => self.pending_chunk = Some(bytes),
                None => {
                    self.eof = true;
                    self.response = None;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
