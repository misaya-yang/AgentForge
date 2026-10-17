//! Internal HTTP response idempotency, ported from ai_gateway_core.comm.idempotency.
//! This is separate from signature nonce replay protection. The enclosing host
//! must verify identity before trusting tenant/user headers used in cache keys.
use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{HeaderName, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use http_body::{Body as HttpBody, Frame, SizeHint};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, StoreError>> + Send + 'a>>;
#[derive(Debug)]
pub struct StoreError;
#[derive(Clone, Serialize, Deserialize)]
pub struct CachedResponse {
    pub status_code: u16,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body_hex: String,
    pub request_body_digest: Option<String>,
}
pub trait IdempotencyStore: Send + Sync {
    fn get_cached<'a>(&'a self, key: &'a str) -> StoreFuture<'a, Option<CachedResponse>>;
    fn try_begin<'a>(&'a self, key: &'a str, ttl: u64) -> StoreFuture<'a, bool>;
    fn store_response<'a>(
        &'a self,
        key: &'a str,
        response: CachedResponse,
        ttl: u64,
    ) -> StoreFuture<'a, ()>;
    fn abort<'a>(&'a self, key: &'a str) -> StoreFuture<'a, ()>;
}
#[derive(Default)]
pub struct InMemoryIdempotencyStore {
    records: Mutex<HashMap<String, (Option<CachedResponse>, Instant)>>,
}
impl IdempotencyStore for InMemoryIdempotencyStore {
    fn get_cached<'a>(&'a self, key: &'a str) -> StoreFuture<'a, Option<CachedResponse>> {
        Box::pin(async move {
            let mut m = self.records.lock().await;
            m.retain(|_, (_, e)| *e > Instant::now());
            Ok(m.get(key).and_then(|(v, _)| v.clone()))
        })
    }
    fn try_begin<'a>(&'a self, key: &'a str, ttl: u64) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let mut m = self.records.lock().await;
            m.retain(|_, (_, e)| *e > Instant::now());
            if m.contains_key(key) {
                return Ok(false);
            }
            let expiry = Instant::now()
                .checked_add(Duration::from_secs(ttl.max(1)))
                .ok_or(StoreError)?;
            m.insert(key.into(), (None, expiry));
            Ok(true)
        })
    }
    fn store_response<'a>(
        &'a self,
        key: &'a str,
        response: CachedResponse,
        ttl: u64,
    ) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let expiry = Instant::now()
                .checked_add(Duration::from_secs(ttl.max(1)))
                .ok_or(StoreError)?;
            self.records
                .lock()
                .await
                .insert(key.into(), (Some(response), expiry));
            Ok(())
        })
    }
    fn abort<'a>(&'a self, key: &'a str) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            self.records.lock().await.remove(key);
            Ok(())
        })
    }
}
pub struct RedisIdempotencyStore {
    client: redis::aio::ConnectionManager,
    prefix: String,
}
impl RedisIdempotencyStore {
    pub fn new(client: redis::aio::ConnectionManager, prefix: &str) -> Self {
        Self {
            client,
            prefix: prefix.trim_end_matches(':').into(),
        }
    }
    fn key(&self, kind: &str, key: &str) -> String {
        format!("{}:{kind}:{key}", self.prefix)
    }
}
impl IdempotencyStore for RedisIdempotencyStore {
    fn get_cached<'a>(&'a self, key: &'a str) -> StoreFuture<'a, Option<CachedResponse>> {
        Box::pin(async move {
            let mut c = self.client.clone();
            let raw: Option<String> = redis::cmd("GET")
                .arg(self.key("response", key))
                .query_async(&mut c)
                .await
                .map_err(|_| StoreError)?;
            raw.filter(|v| !v.is_empty())
                .map(|v| serde_json::from_str(&v).map_err(|_| StoreError))
                .transpose()
        })
    }
    fn try_begin<'a>(&'a self, key: &'a str, ttl: u64) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let mut c = self.client.clone();
            let result: Option<String> = redis::cmd("SET")
                .arg(self.key("lock", key))
                .arg("1")
                .arg("NX")
                .arg("EX")
                .arg(ttl.max(1))
                .query_async(&mut c)
                .await
                .map_err(|_| StoreError)?;
            Ok(result.is_some())
        })
    }
    fn store_response<'a>(
        &'a self,
        key: &'a str,
        response: CachedResponse,
        ttl: u64,
    ) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let mut c = self.client.clone();
            let payload = serde_json::to_string(&response).map_err(|_| StoreError)?;
            let _: () = redis::cmd("SET")
                .arg(self.key("response", key))
                .arg(payload)
                .arg("EX")
                .arg(ttl.max(1))
                .query_async(&mut c)
                .await
                .map_err(|_| StoreError)?;
            self.abort(key).await
        })
    }
    fn abort<'a>(&'a self, key: &'a str) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let mut c = self.client.clone();
            let _: u64 = redis::cmd("DEL")
                .arg(self.key("lock", key))
                .query_async(&mut c)
                .await
                .map_err(|_| StoreError)?;
            Ok(())
        })
    }
}
#[derive(Clone)]
pub struct IdempotencyConfig {
    pub store: Arc<dyn IdempotencyStore>,
    pub ttl_seconds: u64,
    pub wait_timeout: Duration,
    pub wait_poll: Duration,
    pub max_body_bytes: usize,
    pub header_name: HeaderName,
}
impl Default for IdempotencyConfig {
    fn default() -> Self {
        Self {
            store: Arc::new(InMemoryIdempotencyStore::default()),
            ttl_seconds: 86400,
            wait_timeout: Duration::from_secs(5),
            wait_poll: Duration::from_millis(50),
            max_body_bytes: 50 * 1024 * 1024,
            header_name: HeaderName::from_static("idempotency-key"),
        }
    }
}
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|b| char::from(*b)).collect()
}
fn unlatin1(text: &str) -> Result<Vec<u8>, StoreError> {
    text.chars()
        .map(|c| u8::try_from(c as u32).map_err(|_| StoreError))
        .collect()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(text: &str) -> Result<Vec<u8>, StoreError> {
    if text.len() % 2 != 0 {
        return Err(StoreError);
    }
    text.as_bytes()
        .chunks_exact(2)
        .map(|p| {
            let s = std::str::from_utf8(p).map_err(|_| StoreError)?;
            u8::from_str_radix(s, 16).map_err(|_| StoreError)
        })
        .collect()
}
fn plain(status: u16, message: &'static str) -> Response {
    (
        StatusCode::from_u16(status).unwrap(),
        [("content-type", "text/plain; charset=utf-8")],
        message,
    )
        .into_response()
}
fn replay(cached: CachedResponse, digest: &str) -> Result<Response, StoreError> {
    if cached.request_body_digest.as_deref() != Some(digest) {
        return Ok(plain(
            409,
            "Idempotency key was already used with a different request body",
        ));
    }
    let mut r = Response::new(Body::from(unhex(&cached.body_hex)?));
    *r.status_mut() = StatusCode::from_u16(cached.status_code).map_err(|_| StoreError)?;
    for (name, value) in cached.headers {
        if name.eq_ignore_ascii_case("content-length")
            || name.eq_ignore_ascii_case("transfer-encoding")
        {
            continue;
        }
        r.headers_mut().append(
            HeaderName::from_bytes(&unlatin1(&name)?).map_err(|_| StoreError)?,
            HeaderValue::from_bytes(&unlatin1(&value)?).map_err(|_| StoreError)?,
        );
    }
    r.headers_mut()
        .append("x-idempotency-replayed", HeaderValue::from_static("true"));
    Ok(r)
}
struct Lease {
    store: Arc<dyn IdempotencyStore>,
    key: String,
    armed: bool,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if self.armed {
            let store = self.store.clone();
            let key = self.key.clone();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = store.abort(&key).await;
                });
            }
        }
    }
}
pub async fn middleware(
    State(config): State<IdempotencyConfig>,
    request: Request,
    next: Next,
) -> Response {
    match run(config, request, next).await {
        Ok(r) => r,
        Err(_) => plain(500, "Internal Server Error"),
    }
}
async fn run(
    config: IdempotencyConfig,
    request: Request,
    next: Next,
) -> Result<Response, StoreError> {
    if matches!(request.method().as_str(), "GET" | "HEAD" | "OPTIONS") {
        return Ok(next.run(request).await);
    }
    let supplied_key = request
        .headers()
        .get(&config.header_name)
        .filter(|v| !v.as_bytes().is_empty())
        .map(|v| latin1(v.as_bytes()));
    let Some(key) = supplied_key else {
        return Ok(next.run(request).await);
    };
    let key = {
        let hdr = |name: &str| {
            request
                .headers()
                .get(name)
                .map(|v| latin1(v.as_bytes()))
                .unwrap_or_default()
        };
        let identity = format!(
            "{}\n{}\n{}\n{}\n{}",
            request.method(),
            request.uri().path(),
            hdr("x-tenant-id"),
            hdr("x-user-id"),
            key
        );
        hex(&Sha256::digest(identity.as_bytes()))
    };
    let (parts, body) = request.into_parts();
    // Read incrementally to distinguish actual read failure from replay limit.
    let mut stream = body.into_data_stream();
    let mut data = Vec::new();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| StoreError)?;
        if chunk.len() > config.max_body_bytes.saturating_sub(data.len()) {
            return Ok(plain(413, "Idempotency request body is too large"));
        }
        data.extend_from_slice(&chunk);
    }
    let digest = hex(&Sha256::digest(&data));
    if let Some(cached) = config.store.get_cached(&key).await? {
        return replay(cached, &digest);
    }
    if !config.store.try_begin(&key, config.ttl_seconds).await? {
        let deadline = Instant::now()
            .checked_add(config.wait_timeout)
            .ok_or(StoreError)?;
        while Instant::now() < deadline {
            if let Some(cached) = config.store.get_cached(&key).await? {
                return replay(cached, &digest);
            }
            tokio::time::sleep(config.wait_poll.max(Duration::from_millis(1))).await;
        }
        return match config.store.get_cached(&key).await? {
            Some(cached) => replay(cached, &digest),
            None => Ok(plain(409, "Idempotency request still in progress")),
        };
    }
    let lease = Lease {
        store: config.store.clone(),
        key,
        armed: true,
    };
    let response = next.run(Request::from_parts(parts, Body::from(data))).await;
    let (parts, body) = response.into_parts();
    let streaming = parts.headers.get("content-type").is_some_and(|v| {
        latin1(v.as_bytes())
            .to_lowercase()
            .starts_with("text/event-stream")
    });
    let cacheable = !streaming && parts.status.as_u16() < 500;
    let expected_length = if parts.headers.contains_key("transfer-encoding") {
        None
    } else {
        parts
            .headers
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
    };
    let cached = CachedResponse {
        status_code: parts.status.as_u16(),
        headers: parts
            .headers
            .iter()
            .map(|(k, v)| (k.as_str().into(), latin1(v.as_bytes())))
            .collect(),
        body_hex: String::new(),
        request_body_digest: Some(digest),
    };
    if body.is_end_stream() || (expected_length == Some(0) && body.size_hint().lower() == 0) {
        let cacheable = cacheable && expected_length.is_none_or(|n| n == 0);
        complete(lease, cached, cacheable, config.ttl_seconds).await?;
        return Ok(Response::from_parts(parts, body));
    }
    Ok(Response::from_parts(
        parts,
        Body::new(CaptureBody {
            inner: body,
            lease: Some(lease),
            cached: Some(cached),
            chunks: Vec::new(),
            expected_length,
            received_bytes: 0,
            cacheable,
            ttl: config.ttl_seconds,
            finish: None,
            pending_final: None,
            done: false,
        }),
    ))
}
struct CaptureBody {
    inner: Body,
    lease: Option<Lease>,
    cached: Option<CachedResponse>,
    chunks: Vec<u8>,
    expected_length: Option<u64>,
    received_bytes: u64,
    cacheable: bool,
    ttl: u64,
    finish: Option<StoreFuture<'static, ()>>,
    pending_final: Option<Frame<Bytes>>,
    done: bool,
}
impl HttpBody for CaptureBody {
    type Data = Bytes;
    type Error = axum::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        if self.done {
            return Poll::Ready(None);
        }
        if let Some(future) = self.finish.as_mut() {
            return match future.as_mut().poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(result) => {
                    self.done = true;
                    match result {
                        Ok(()) => Poll::Ready(self.pending_final.take().map(Ok)),
                        Err(_) => Poll::Ready(Some(Err(axum::Error::new(std::io::Error::other(
                            "idempotency_store_failed",
                        ))))),
                    }
                }
            };
        }
        match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    self.received_bytes = self.received_bytes.saturating_add(data.len() as u64);
                    if self
                        .expected_length
                        .is_some_and(|n| self.received_bytes > n)
                    {
                        self.cacheable = false;
                    }
                    if self.cacheable {
                        self.chunks.extend_from_slice(data);
                    }
                }
                if self.inner.is_end_stream() || self.expected_length == Some(self.received_bytes) {
                    self.pending_final = Some(frame);
                    self.begin_finish();
                    return self.poll_frame(cx);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => {
                self.done = true;
                self.lease.take();
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                self.begin_finish();
                self.poll_frame(cx)
            }
        }
    }
    fn is_end_stream(&self) -> bool {
        self.done
    }
    // Completion is also initiated on a known final frame, since HTTP transports
    // may stop polling after satisfying an explicit Content-Length.
    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}

async fn complete(
    mut lease: Lease,
    cached: CachedResponse,
    cacheable: bool,
    ttl: u64,
) -> Result<(), StoreError> {
    let result = if cacheable {
        lease.store.store_response(&lease.key, cached, ttl).await
    } else {
        lease.store.abort(&lease.key).await
    };
    if result.is_ok() {
        lease.armed = false;
    }
    result
}
impl CaptureBody {
    fn begin_finish(&mut self) {
        if self
            .expected_length
            .is_some_and(|n| n != self.received_bytes)
        {
            self.cacheable = false;
        }
        let lease = self.lease.take().expect("owned response lease");
        let mut cached = self.cached.take().expect("response cache metadata");
        cached.body_hex = hex(&self.chunks);
        self.chunks.clear();
        self.finish = Some(Box::pin(complete(lease, cached, self.cacheable, self.ttl)));
    }
}
