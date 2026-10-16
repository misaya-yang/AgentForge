use super::*;
use axum::{
    body::{Body, Bytes},
    http::{HeaderName, HeaderValue, StatusCode},
};
use futures_core::Stream;
use std::{
    collections::BTreeSet,
    pin::Pin,
    sync::Mutex,
    task::{Context, Poll},
    time::Instant,
};
/// RPC applies only to the registered unary retrieve route; no wildcard upgrade.
fn retrieval_rpc_dataset<'a>(method: &Method, path: &'a str) -> Option<&'a str> {
    if method != Method::POST {
        return None;
    }
    let (dataset, operation) = path.split_once('/')?;
    (operation == "retrieve" && !dataset.is_empty()).then_some(dataset)
}

#[cfg(test)]
mod rpc_selection_tests {
    use super::*;
    #[test]
    fn only_registered_unary_retrieve_selects_rpc() {
        assert_eq!(
            retrieval_rpc_dataset(&Method::POST, "dataset-id/retrieve"),
            Some("dataset-id")
        );
        for path in [
            "retrieve",
            "/retrieve",
            "d/retrieve/",
            "d/retrieve_batch",
            "d/qa/stream",
            "datasets/d/retrieve",
            "d/documents",
        ] {
            assert_eq!(retrieval_rpc_dataset(&Method::POST, path), None);
        }
        assert_eq!(retrieval_rpc_dataset(&Method::GET, "d/retrieve"), None);
    }
}
#[derive(Default)]
pub(super) struct Breaker {
    state: Mutex<(usize, Option<Instant>)>,
}
impl Breaker {
    fn gate(&self) -> Result<()> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.0 >= 3 {
            if s.1.is_some_and(|t| t.elapsed() < Duration::from_secs(30)) {
                let mut e = ContentError::new(503, "KB Service recovering — probe in flight");
                e.headers.insert("Retry-After".into(), "2".into());
                return Err(e);
            }
            s.1 = Some(Instant::now());
        }
        Ok(())
    }
    fn response(&self, status: StatusCode) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if status.is_success() {
            s.0 = 0;
        } else if status.is_server_error() {
            s.0 = s.0.saturating_add(1);
        }
        s.1 = None;
    }
    fn failure(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.0 = s.0.saturating_add(1);
        s.1 = None;
    }
}
fn strip(headers: &HeaderMap, request: bool) -> HeaderMap {
    let mut blocked: BTreeSet<String> = [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for value in headers.get_all("connection") {
        if let Ok(v) = value.to_str() {
            blocked.extend(v.split(',').map(|s| s.trim().to_lowercase()));
        }
    }
    if request {
        blocked.extend(
            [
                "host",
                "content-length",
                "authorization",
                "x-api-key",
                "cookie",
                "x-gateway-secret",
                "x-app-user-id",
                "x-app-tenant-id",
                "x-user-id",
                "x-tenant-id",
                "x-user-tier",
                "x-user-type",
                "x-user-roles",
                "x-user-email",
                "x-user-name",
            ]
            .into_iter()
            .map(str::to_owned),
        );
    }
    let mut result = HeaderMap::new();
    for (k, v) in headers {
        if !blocked.contains(k.as_str()) {
            result.append(k.clone(), v.clone());
        }
    }
    result
}
pub(super) fn trusted_headers(user: &ContentIdentity, incoming: &HeaderMap) -> Result<HeaderMap> {
    let mut headers = strip(incoming, true);
    for (name, value) in [
        ("x-user-id", user.user_id.clone()),
        ("x-tenant-id", user.tenant_id.clone()),
        ("x-user-tier", user.tier.clone()),
        (
            "x-user-type",
            if user.is_authenticated {
                "user"
            } else if user.user_id.starts_with("guest_") {
                "guest"
            } else {
                "anonymous"
            }
            .into(),
        ),
        ("x-user-roles", user.roles.join(",")),
    ] {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_str(&value)
                .map_err(|_| ContentError::new(400, "Invalid identity"))?,
        );
    }
    // Explicit identity encoding keeps response bytes and content-length coherent.
    headers.insert("accept-encoding", HeaderValue::from_static("identity"));
    Ok(headers)
}
// This is the same request consumed by the transport below. Keeping construction
// separate permits cross-language contract checks without a listening service.
use super::contract::public_upstream_path;
#[allow(clippy::too_many_arguments)]
fn signed_request(
    client: &reqwest::Client,
    origin: &reqwest::Url,
    signer: &GatewayAuth,
    api_key_header: &str,
    method: Method,
    path: &str,
    query: &str,
    mut headers: HeaderMap,
    body: Vec<u8>,
) -> Result<reqwest::Request> {
    let mut url = origin.clone();
    url.set_path(path);
    url.set_query((!query.is_empty()).then_some(query));
    headers.remove(api_key_header);
    let signature = signer
        .sign(
            method.as_str(),
            url.path(),
            url.query().unwrap_or(""),
            &body,
            &headers,
        )
        .map_err(|_| unavailable())?;
    headers.insert(
        "x-gateway-secret",
        HeaderValue::from_str(&signature).map_err(|_| unavailable())?,
    );
    client
        .request(method, url)
        .headers(headers)
        .body(body)
        .build()
        .map_err(|_| ContentError::new(502, "KB Service unavailable"))
}
#[cfg(test)]
#[path = "hybrid_contract_tests.rs"]
mod hybrid_contract_tests;
impl KnowledgeProxy {
    pub(super) async fn upstream(
        &self,
        method: Method,
        path: &str,
        query: &str,
        headers: HeaderMap,
        body: Vec<u8>,
    ) -> Result<reqwest::Response> {
        for attempt in 0..2 {
            let request = signed_request(
                &self.client,
                &self.config.origin,
                &self.signer,
                self.access.identities.api_key_header.as_str(),
                method.clone(),
                path,
                query,
                headers.clone(),
                body.clone(),
            )?;
            match self.client.execute(request).await {
                Ok(response) => return Ok(response),
                Err(error) if attempt == 0 && error.is_connect() => {
                    tokio::time::sleep(Duration::from_millis(50)).await
                }
                Err(error) => {
                    return Err(ContentError::new(
                        if error.is_timeout() { 504 } else { 502 },
                        if error.is_timeout() {
                            "KB Service timeout"
                        } else {
                            "KB Service unavailable"
                        },
                    ));
                }
            }
        }
        Err(unavailable())
    }
    pub(super) async fn forward(
        &self,
        user: ContentIdentity,
        method: Method,
        path: String,
        query: String,
        incoming: HeaderMap,
        body: Vec<u8>,
    ) -> Result<Response> {
        let permit = tokio::time::timeout(
            Duration::from_secs(30),
            self.connections.clone().acquire_owned(),
        )
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
        let upstream_path = public_upstream_path(&path);
        let key = crate::migration_compat::route_pattern::extract_route_pattern(&upstream_path);
        let breaker = {
            let mut map = self.breakers.lock().unwrap_or_else(|e| e.into_inner());
            let key = if map.len() >= 1024 && !map.contains_key(&key) {
                "{overflow}".into()
            } else {
                key
            };
            map.entry(key).or_default().clone()
        };
        breaker.gate()?;
        let headers = trusted_headers(&user, &incoming)?;
        let mut observation = self
            .trace_capture
            .as_ref()
            .filter(|_| crate::migration_knowledge_query::rag_trace::is_retrieve_path(&path))
            .map(|config| Observation {
                config: config.clone(),
                user,
                path: path.clone(),
                request_id: incoming
                    .get("x-request-id")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .into(),
                traceparent: incoming
                    .get("traceparent")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned),
                query: crate::migration_knowledge_query::rag_trace::parse_request(&body),
                started: chrono::Utc::now(),
                status: 502,
                captured: vec![],
                truncated: false,
            });
        // Only this exact unary operation opts into RPC. Its original signed REST
        // path and bytes remain the Knowledge owner's authorization input.
        if let Some(rpc) = &self.retrieval_rpc {
            if let Some(dataset_id) = retrieval_rpc_dataset(&method, &path) {
                if !query.is_empty() {
                    return Err(ContentError::new(
                        400,
                        "Retrieval query parameters are not supported over RPC",
                    ));
                }
                let message =
                    ai_platform_rpc::request_from_json(dataset_id.to_owned(), body.clone())
                        .map_err(|_| ContentError::new(422, "Invalid retrieval request"))?;
                let signed = signed_request(
                    &self.client,
                    &self.config.origin,
                    &self.signer,
                    self.access.identities.api_key_header.as_str(),
                    method.clone(),
                    &upstream_path,
                    "",
                    headers,
                    body,
                )?;
                let cancel = ai_platform_rpc::CancellationToken::new();
                let reply = rpc.retrieve(message, signed.headers(), &cancel).await;
                let reply = match reply {
                    Ok(reply) => reply,
                    Err(error) => {
                        breaker.failure();
                        let status = match error.code() {
                            ai_platform_rpc::tonic::Code::DeadlineExceeded => 504,
                            ai_platform_rpc::tonic::Code::ResourceExhausted => 503,
                            ai_platform_rpc::tonic::Code::Cancelled => 499,
                            _ => 502,
                        };
                        if let Some(mut o) = observation {
                            o.status = status;
                            o.finish();
                        }
                        return Err(ContentError::new(
                            status,
                            if status == 504 {
                                "KB Service timeout"
                            } else {
                                "KB Service unavailable"
                            },
                        ));
                    }
                };
                let status = u16::try_from(reply.status_code)
                    .ok()
                    .and_then(|v| StatusCode::from_u16(v).ok())
                    .ok_or_else(unavailable)?;
                breaker.response(status);
                if let Some(mut o) = observation {
                    o.status = status.as_u16();
                    o.append(&reply.response_json);
                    o.finish();
                }
                let mut response = Response::new(Body::from(reply.response_json));
                *response.status_mut() = status;
                response
                    .headers_mut()
                    .insert("content-type", HeaderValue::from_static("application/json"));
                if let Some(value) = reply
                    .retry_after
                    .and_then(|v| HeaderValue::from_str(&v).ok())
                {
                    response.headers_mut().insert("retry-after", value);
                }
                drop(permit);
                return Ok(response);
            }
        }
        // Replayable bytes are retained only through the HTTP response headers.
        // Retry only connect failures: a possibly committed mutation is never replayed.
        let response = self
            .upstream(method, &upstream_path, &query, headers, body)
            .await;
        let response = match response {
            Ok(r) => r,
            Err(e) => {
                breaker.failure();
                if let Some(mut o) = observation {
                    o.status = e.status;
                    o.finish();
                }
                return Err(e);
            }
        };
        let status = response.status();
        breaker.response(status);
        if let Some(o) = &mut observation {
            o.status = status.as_u16();
        }
        let mut headers = strip(response.headers(), false);
        let sse = headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_lowercase().starts_with("text/event-stream"));
        if sse {
            headers.remove("content-length");
        }
        // Every response streams with bounded backpressure, including unexpected
        // large bodies advertising a small Content-Length. SSE never buffers.
        let stream = ForwardStream {
            inner: Box::pin(response.bytes_stream()),
            breaker,
            observation,
            permit: Some(permit),
            finished: false,
        };
        let mut result = Response::new(Body::from_stream(stream));
        *result.status_mut() = status;
        *result.headers_mut() = headers;
        result
            .headers_mut()
            .insert("cache-control", HeaderValue::from_static("no-store"));
        Ok(result)
    }
}
struct Observation {
    config: crate::migration_knowledge_query::rag_trace::RagTraceCapture,
    user: ContentIdentity,
    path: String,
    request_id: String,
    traceparent: Option<String>,
    query: String,
    started: chrono::DateTime<chrono::Utc>,
    status: u16,
    captured: Vec<u8>,
    truncated: bool,
}
impl Observation {
    fn append(&mut self, b: &[u8]) {
        let left = (256 * 1024usize).saturating_sub(self.captured.len());
        if b.len() > left {
            self.truncated = true;
        }
        self.captured.extend_from_slice(&b[..b.len().min(left)]);
    }
    fn finish(self) {
        if self.truncated {
            return;
        }
        use crate::migration_knowledge_query::rag_trace as rag;
        let (results, metadata) = rag::parse_response(&self.captured);
        let documents = rag::retrieval_documents(
            &results,
            self.config.eval_context_max_chunks,
            self.config.eval_context_max_chars,
        );
        let dataset = rag::dataset_id(&self.path);
        let trace_id = metadata["trace_id"].as_str().map(str::to_owned);
        let success = (200..400).contains(&self.status);
        let error = (!success).then(|| format!("upstream_status={}", self.status));
        let trace = rag::build_rag_trace(rag::RagTraceInput {
            request_id: &self.request_id,
            tenant_id: &self.user.tenant_id,
            user_id: &self.user.user_id,
            dataset_id: dataset.as_deref(),
            query: &self.query,
            started: self.started,
            ended: chrono::Utc::now(),
            status: if success { "succeeded" } else { "failed" },
            upstream_status: Some(self.status),
            document_count: documents.len(),
            error: error.as_deref(),
            traceparent: self.traceparent.as_deref(),
            documents,
            metadata,
            trace_id: trace_id.as_deref(),
        });
        self.config.repository.schedule_gateway_trace_ingest(
            trace,
            self.config.retention_days,
            true,
        );
    }
}
struct ForwardStream {
    inner: Pin<Box<dyn Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send>>,
    breaker: Arc<Breaker>,
    observation: Option<Observation>,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    finished: bool,
}
impl Stream for ForwardStream {
    type Item = std::result::Result<Bytes, reqwest::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let next = self.inner.as_mut().poll_next(cx);
        match &next {
            Poll::Ready(Some(Ok(bytes))) => {
                if let Some(o) = &mut self.observation {
                    o.append(bytes);
                }
            }
            Poll::Ready(Some(Err(_))) => {
                self.breaker.failure();
                self.observation = None;
                self.finished = true;
                self.permit = None;
            }
            Poll::Ready(None) => {
                if let Some(o) = self.observation.take() {
                    o.finish();
                }
                self.finished = true;
                self.permit = None;
            }
            _ => {}
        }
        next
    }
}
impl Drop for ForwardStream {
    fn drop(&mut self) {
        if !self.finished {
            self.observation = None;
        }
    }
}
