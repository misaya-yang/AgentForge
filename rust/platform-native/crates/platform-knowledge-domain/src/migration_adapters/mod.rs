use ai_platform_service_primitives::transport_error::{AdapterFailure, DispatchError, AdapterError};
use reqwest::{Response, Url};
use serde_json::Value;
use tokio::sync::OwnedSemaphorePermit;
use std::{collections::HashSet, net::{IpAddr, SocketAddr}, time::Duration};
fn validation(message: &'static str) -> AdapterError { DispatchError::AdapterValidationFailed(message).into() }
pub struct HttpResponse {
    pub response: Response,
    pub permit: Option<OwnedSemaphorePermit>,
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

fn unsafe_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let n = u32::from(v);
            v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_multicast()
                || v.is_unspecified()
                || v.is_broadcast()
                || v.is_documentation()
                || n >> 24 == 0
                || n >> 28 == 15
                || (n & 0xffffff00) == 0xc0000000
                || (n & 0xffffff00) == 0xc0586300
                || (n & 0xfffe0000) == 0xc6120000
        }
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return unsafe_ip(IpAddr::V4(v4));
            }
            let s = v.segments();
            v.is_loopback()
                || v.is_unspecified()
                || v.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] & 0xe000) != 0x2000
                || (s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                || s[0] == 0x2002
        }
    }
}
pub async fn validated_destination(url: &str) -> Result<(Url, Vec<SocketAddr>), AdapterError> {
    let url = Url::parse(url).map_err(|_| validation("URL must be http(s)"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(validation("URL must be http(s)"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| validation("URL missing host"))?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let port = url
        .port_or_known_default()
        .ok_or_else(|| validation("URL missing port"))?;
    let addresses: Vec<_> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| validation("destination DNS resolution failed"))?
        .collect();
    if addresses.is_empty() || addresses.iter().any(|addr| unsafe_ip(addr.ip())) {
        return Err(validation("destination rejected"));
    }
    Ok((url, addresses))
}
pub(crate) async fn safe_fetch(raw: &str, max_bytes: usize) -> Result<Vec<u8>, AdapterError> {
    safe_fetch_with_policy(raw, max_bytes, 3, Duration::from_secs(30)).await
}
/// Fetch a user-controlled URL with DNS pinning and independent validation at
/// every redirect. Never uses service credentials or environment HTTP proxies.
/// The timeout applies to connection establishment and each response read, as
/// the canonical httpx timeout does, rather than imposing a whole-body deadline.
pub async fn safe_fetch_with_policy(
    raw: &str,
    max_bytes: usize,
    max_redirects: usize,
    timeout: Duration,
) -> Result<Vec<u8>, AdapterError> {
    if max_bytes == 0 {
        return Err(validation("max_bytes must be positive"));
    }
    let mut current = raw.to_owned();
    let mut seen = HashSet::new();
    for _ in 0..=max_redirects {
        if !seen.insert(current.clone()) {
            return Err(validation("redirect loop detected"));
        }
        let (url, addresses) = validated_destination(&current).await?;
        let host = url
            .host_str()
            .ok_or_else(|| validation("URL missing host"))?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(timeout)
            .read_timeout(timeout)
            .resolve_to_addrs(host, &addresses[..1])
            .build()
            .map_err(|_| validation("safe fetch client failed"))?;
        let mut response = client
            .get(url.clone())
            .send()
            .await
            .map_err(|_| validation("failed to fetch URL"))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| validation("redirect without Location header"))?;
            current = url
                .join(location)
                .map_err(|_| validation("invalid redirect URL"))?
                .to_string();
            continue;
        }
        if !response.status().is_success() {
            return Err(validation("failed to fetch URL"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| validation("failed to fetch URL"))?
        {
            if bytes.len().saturating_add(chunk.len()) > max_bytes {
                return Err(DispatchError::AdapterValidationDetail(format!(
                    "payload exceeds {} MB cap",
                    max_bytes / (1024 * 1024)
                ))
                .into());
            }
            bytes.extend(chunk);
        }
        return Ok(bytes);
    }
    Err(
        DispatchError::AdapterValidationDetail(format!("exceeded {max_redirects} redirects"))
            .into(),
    )
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
