use super::*;
use crate::{capacity_admission::AdmissionCancellation, dispatcher::DispatchSettlement};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Path, Request, State},
    routing::{get, post},
};
use futures_core::Stream;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll},
};
use tokio::sync::{mpsc, oneshot};
pub fn router(api: LlmApi) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/messages", post(messages))
        .route("/v1/models", get(models))
        .route("/v1/models/{*model}", get(model))
        .with_state(api)
}
async fn models(State(api): State<LlmApi>, request: Request) -> LlmResult<axum::Json<Value>> {
    let actor = api
        .backend
        .authenticate(request.headers(), request.uri().path())
        .await?;
    Ok(axum::Json(api.backend.models(&actor).await?))
}
async fn model(
    State(api): State<LlmApi>,
    Path(id): Path<String>,
    request: Request,
) -> LlmResult<axum::Json<Value>> {
    let actor = api
        .backend
        .authenticate(request.headers(), request.uri().path())
        .await?;
    let models = api.backend.models(&actor).await?;
    let found = models["data"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["id"] == id))
        .cloned()
        .ok_or(LlmError::new(404, "llm_model_not_found"))?;
    Ok(axum::Json(found))
}
async fn chat(State(api): State<LlmApi>, request: Request) -> LlmResult<Response> {
    handle(api, Surface::Chat, request).await
}
async fn embeddings(State(api): State<LlmApi>, request: Request) -> LlmResult<Response> {
    handle(api, Surface::Embeddings, request).await
}
async fn messages(State(api): State<LlmApi>, request: Request) -> LlmResult<Response> {
    handle(api, Surface::Messages, request).await
}
struct CancelOnDrop(Option<AdmissionCancellation>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            cancel.cancel()
        }
    }
}
async fn handle(api: LlmApi, surface: Surface, request: Request) -> LlmResult<Response> {
    let (parts, body) = request.into_parts();
    if parts.uri.query().is_some() {
        return Err(LlmError::new(400, "llm_query_unsupported"));
    }
    if parts
        .headers
        .get("content-encoding")
        .is_some_and(|v| v != "identity")
    {
        return Err(LlmError::new(415, "llm_content_encoding_unsupported"));
    }
    let actor = api
        .backend
        .authenticate(&parts.headers, surface.path())
        .await?;
    let body = to_bytes(body, 32 * 1024 * 1024)
        .await
        .map_err(|_| LlmError::new(413, "llm_request_too_large"))?;
    let mut invocation = codec::parse(surface, body)?;
    invocation.client_request_id = parts
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| uuid::Uuid::parse_str(v).is_ok())
        .map(str::to_owned);
    invocation.external_audit = parts
        .headers
        .get("x-agentforge-a2a-task-id")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| uuid::Uuid::parse_str(v).is_ok());
    let audit_requested = invocation.external_audit;
    let client_request_id = invocation.client_request_id.clone();
    let request_sha256 = format!("{:x}", Sha256::digest(&invocation.body));
    let request_id = invocation.request_id.clone();
    let prepared = match api
        .backend
        .prepare(&actor, invocation, &parts.headers)
        .await
    {
        Ok(value) => value,
        Err(error) => {
            if audit_requested {
                wire_audit(
                    &api,
                    &actor.tenant,
                    &actor.user,
                    &request_id,
                    None,
                    "denied",
                    error.status,
                    client_request_id,
                    Some(request_sha256),
                )
                .await?;
            }
            let status = error.status;
            let (parts, body) = error.into_response().into_parts();
            return Ok(response(status, parts.headers, body, &request_id));
        }
    };
    let cancel = AdmissionCancellation::default();
    let mut guard = CancelOnDrop(Some(cancel.clone()));
    let (reply, rx) = oneshot::channel();
    tokio::spawn(run(api, prepared, cancel, reply));
    let response = rx
        .await
        .map_err(|_| LlmError::new(502, "llm_upstream_outcome_unknown"))?;
    guard.0.take(); // A streaming body now owns cancellation; nonstream is complete.
    Ok(response)
}
struct ByteStream {
    rx: mpsc::Receiver<Bytes>,
    cancel: AdmissionCancellation,
    completed: Arc<AtomicBool>,
    ended: bool,
}
impl Stream for ByteStream {
    type Item = Result<Bytes, io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.ended {
            return Poll::Ready(None);
        }
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(bytes)) => Poll::Ready(Some(Ok(bytes))),
            Poll::Ready(None) => {
                self.ended = true;
                if self.completed.load(Ordering::Acquire) {
                    Poll::Ready(None)
                } else {
                    Poll::Ready(Some(Err(io::Error::other("llm_stream_outcome_unknown"))))
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
impl Drop for ByteStream {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
fn response(status: u16, mut headers: HeaderMap, body: Body, id: &str) -> Response {
    if let Ok(value) = id.parse() {
        headers.insert("x-gateway-request-id", value);
    }
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
        headers,
        body,
    )
        .into_response()
}
async fn run(
    api: LlmApi,
    prepared: Prepared,
    cancel: AdmissionCancellation,
    reply: oneshot::Sender<Response>,
) {
    let mut reply = Some(reply);
    if prepared.lease.bind_owner(cancel.clone()).is_err() || cancel.is_cancelled() {
        let _ = prepared
            .lease
            .mark_completion(DispatchSettlement::NotDispatched)
            .await;
        if let Some(reply) = reply.take() {
            let _ =
                reply.send(LlmError::new(503, "llm_dispatch_owner_unavailable").into_response());
        }
        let _ = prepared.lease.release().await;
        return;
    }
    // This CAS is acknowledged before the first provider future can be polled.
    let marked = prepared.lease.mark_dispatched().await;
    let outcome = if marked.is_err() {
        Err(LlmError::new(503, "llm_dispatch_state_unknown"))
    } else {
        let proof = prepared.lease.quota_admission();
        let work = crate::capacity_admission::atomic::quota_scope(
            proof,
            pipeline(&api, &prepared, &cancel, &mut reply),
        );
        tokio::select! {biased;
            _=cancel.cancelled()=>Err(LlmError::new(502,"llm_request_cancelled_unknown")),
            _=prepared.lease.wait_lost()=>Err(LlmError::new(503,"llm_dispatch_lease_lost")),
            result=tokio::time::timeout(prepared.timeout,work)=>result.unwrap_or(Err(LlmError::new(504,"llm_deadline_outcome_unknown"))),
        }
    };
    if let Err(ref error) = outcome {
        if prepared.invocation.external_audit {
            let _ = wire_audit(
                &api,
                &prepared.record.tenant_id,
                &prepared.record.user_id,
                &prepared.invocation.request_id,
                None,
                "unknown",
                error.status,
                prepared.invocation.client_request_id.clone(),
                Some(format!("{:x}", Sha256::digest(&prepared.invocation.body))),
            )
            .await;
        }
        let _ = prepared
            .lease
            .mark_completion(DispatchSettlement::Unknown)
            .await;
    }
    if let Some(reply) = reply.take() {
        let _ = reply.send(
            outcome
                .err()
                .unwrap_or(LlmError::new(502, "llm_upstream_outcome_unknown"))
                .into_response(),
        );
    }
    let _ = prepared.lease.release().await;
}
async fn commit(
    api: &LlmApi,
    prepared: &Prepared,
    observed: meter::Observation,
    status: u16,
) -> LlmResult<()> {
    prepared
        .lease
        .mark_completion(DispatchSettlement::Completed)
        .await
        .map_err(|_| LlmError::new(503, "llm_completion_state_unknown"))?;
    api.backend.settle(prepared, observed, status).await
}
async fn pipeline(
    api: &LlmApi,
    prepared: &Prepared,
    cancel: &AdmissionCancellation,
    reply: &mut Option<oneshot::Sender<Response>>,
) -> LlmResult<()> {
    let holdback_start = std::time::Instant::now();
    let _holdback = if prepared.invocation.stream {
        if let Some(g) = prepared.guardrails.as_ref().filter(|g| g.output_enabled()) {
            Some(g.buffer_permit().await?)
        } else {
            None
        }
    } else {
        None
    };
    let mut upstream = api.transport.send(prepared).await?;
    let mut headers = transport::response_headers(&upstream.headers);
    if let Some(id) = &prepared.route_decision {
        headers.insert("x-route-decision", id.clone());
    }
    let streaming = prepared.invocation.stream && (200..300).contains(&upstream.status);
    if !streaming {
        let mut bytes = Vec::new();
        while let Some(chunk) = upstream.body.next().await {
            let chunk = chunk?;
            if bytes.len().saturating_add(chunk.len()) > 32 * 1024 * 1024 {
                return Err(LlmError::new(502, "llm_response_too_large"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let screened = if let Some(g) = prepared.guardrails.as_ref().filter(|g| g.output_enabled())
        {
            g.inspect_output(&bytes, false, 0).await
        } else {
            Ok(())
        };
        if let Some(observed) = meter::non_stream(
            prepared.invocation.surface,
            &prepared.record.provider,
            &bytes,
        ) {
            commit(api, prepared, observed, upstream.status).await?;
        } else {
            prepared
                .lease
                .mark_completion(DispatchSettlement::Unknown)
                .await
                .map_err(|_| LlmError::new(503, "llm_completion_state_unknown"))?;
            if (200..300).contains(&upstream.status) {
                return Err(LlmError::new(502, "llm_terminal_usage_unknown"));
            }
        }
        let wire_id = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v["id"].as_str().map(str::to_owned));
        if prepared.invocation.external_audit {
            wire_audit(
                api,
                &prepared.record.tenant_id,
                &prepared.record.user_id,
                &prepared.invocation.request_id,
                wire_id,
                if screened.is_err() {
                    "denied"
                } else if (200..300).contains(&upstream.status) {
                    "completed"
                } else {
                    "denied"
                },
                upstream.status,
                prepared.invocation.client_request_id.clone(),
                Some(format!("{:x}", Sha256::digest(&prepared.invocation.body))),
            )
            .await?;
        }
        if let Err(error) = screened {
            if let Some(reply) = reply.take() {
                let _ = reply.send(error.into_response());
            }
            return Ok(());
        }
        if let Some(reply) = reply.take() {
            let _ = reply.send(response(
                upstream.status,
                headers,
                Body::from(bytes),
                &prepared.invocation.request_id,
            ));
        }
        return Ok(());
    }
    if !upstream
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| {
            s.split(';')
                .next()
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("text/event-stream"))
        })
    {
        return Err(LlmError::new(502, "llm_stream_content_type_invalid"));
    }
    if let Some(g) = prepared.guardrails.as_ref().filter(|g| g.output_enabled()) {
        let mut bytes = Vec::new();
        let choices = prepared.invocation.payload["n"].as_u64().unwrap_or(1);
        let mut meter = meter::StreamMeter::new(
            prepared.invocation.surface,
            prepared.record.provider.clone(),
            choices,
        );
        while let Some(chunk) = upstream.body.next().await {
            let chunk = chunk?;
            if bytes.len().saturating_add(chunk.len()) > g.output_limit() {
                let holdback =
                    u64::try_from(holdback_start.elapsed().as_micros()).unwrap_or(u64::MAX);
                g.reject_output_limit(holdback).await?;
                return Err(LlmError::new(502, "llm_guardrails_output_limit"));
            }
            bytes.extend_from_slice(&chunk);
            if let Some(observed) = meter.push(&chunk)? {
                let holdback =
                    u64::try_from(holdback_start.elapsed().as_micros()).unwrap_or(u64::MAX);
                let screened = g.inspect_output(&bytes, true, holdback).await;
                // Provider execution is complete and remains billable even if
                // its output is rejected. Never mislabel this as not dispatched.
                commit(api, prepared, observed, upstream.status).await?;
                if let Some(reply) = reply.take() {
                    let response = match screened {
                        Ok(()) => response(
                            upstream.status,
                            headers,
                            Body::from(bytes),
                            &prepared.invocation.request_id,
                        ),
                        Err(error) => error.into_response(),
                    };
                    let _ = reply.send(response);
                }
                return Ok(());
            }
        }
        return Err(LlmError::new(502, "llm_stream_terminal_missing"));
    }
    let (tx, rx) = mpsc::channel(8);
    let completed = Arc::new(AtomicBool::new(false));
    let body = Body::from_stream(ByteStream {
        rx,
        cancel: cancel.clone(),
        completed: completed.clone(),
        ended: false,
    });
    if reply.take().is_none_or(|reply| {
        reply
            .send(response(
                upstream.status,
                headers,
                body,
                &prepared.invocation.request_id,
            ))
            .is_err()
    }) {
        return Err(LlmError::new(502, "llm_request_cancelled_unknown"));
    }
    let choices = prepared.invocation.payload["n"].as_u64().unwrap_or(1);
    let mut meter = meter::StreamMeter::new(
        prepared.invocation.surface,
        prepared.record.provider.clone(),
        choices,
    );
    while let Some(chunk) = upstream.body.next().await {
        let chunk = chunk?;
        if let Some(observed) = meter.push(&chunk)? {
            // Do not expose a native success terminator until actual usage has
            // reached the same durable raw/checkpoint path as other callers.
            commit(api, prepared, observed, upstream.status).await?;
            completed.store(true, Ordering::Release);
            let _ = tx.send(chunk).await;
            return Ok(());
        }
        tx.send(chunk)
            .await
            .map_err(|_| LlmError::new(502, "llm_request_cancelled_unknown"))?;
    }
    Err(LlmError::new(502, "llm_stream_terminal_missing"))
}

async fn wire_audit(
    api: &LlmApi,
    tenant: &str,
    user: &str,
    request: &str,
    wire: Option<String>,
    outcome: &str,
    status: u16,
    client_request_id: Option<String>,
    request_sha256: Option<String>,
) -> LlmResult<()> {
    if let Some(pool) = &api.audit_pool {
        ai_platform_gateway_foundation::external_a2a_ledger::record(
            pool,
            tenant,
            user,
            &ai_platform_gateway_foundation::external_a2a_ledger::Receipt {
                kind: ai_platform_gateway_foundation::external_a2a_ledger::Kind::Model,
                request_id: request.into(),
                client_request_id,
                request_sha256,
                wire_response_id: wire,
                tool: None,
                local_message_id: None,
                remote_task_id: None,
                outcome: outcome.into(),
                http_status: status,
            },
        )
        .await
        .map_err(|_| LlmError::new(503, "llm_wire_audit_unconfirmed"))?;
    }
    Ok(())
}
