//! Record exact SSE bytes once and persist only a fully consumed bounded body.
use super::{AgentError, AgentFuture, public::RuntimeIdempotency};
use axum::{
    body::{Body, BodyDataStream, Bytes},
    response::Response,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_core::Stream;
use serde_json::Value;
use sqlx::PgPool;
use std::{
    pin::Pin,
    task::{Context, Poll},
};
pub struct Reservation {
    pub key: RuntimeIdempotency,
    pub pool: PgPool,
    pub armed: bool,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if self.armed {
            let key = self.key.clone();
            let pool = self.pool.clone();
            tokio::spawn(async move {
                let _ = key.fail(&pool).await;
            });
        }
    }
}
pub fn replay(row: &Value) -> Result<Response, AgentError> {
    let status = row["status"].as_str().unwrap_or("pending");
    if status != "completed" || row["response_body"].is_null() {
        return Err(AgentError::code(
            409,
            if status == "pending" {
                "AGENT_RUNTIME_IDEMPOTENCY_IN_PROGRESS"
            } else {
                "AGENT_RUNTIME_IDEMPOTENCY_EXECUTION_FAILED"
            },
        ));
    }
    let body = STANDARD
        .decode(
            row["response_body"]
                .as_str()
                .unwrap_or("")
                .split_whitespace()
                .collect::<String>(),
        )
        .map_err(|_| AgentError::code(503, "AGENT_RUNTIME_IDEMPOTENCY_STATE_INVALID"))?;
    Response::builder()
        .status(row["response_status_code"].as_u64().unwrap_or(200) as u16)
        .header(
            "content-type",
            row["response_media_type"]
                .as_str()
                .unwrap_or("text/event-stream"),
        )
        .header("cache-control", "no-cache")
        .header("x-session-id", row["session_id"].as_str().unwrap_or(""))
        .header("x-idempotent-replay", "true")
        .body(Body::from(body))
        .map_err(|_| AgentError::code(503, "AGENT_RUNTIME_IDEMPOTENCY_STATE_INVALID"))
}
pub fn record(response: Response, mut reservation: Reservation, max: usize) -> Response {
    let (parts, body) = response.into_parts();
    let media = parts
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/event-stream")
        .to_owned();
    let stream = Recording {
        source: body.into_data_stream(),
        buffer: Vec::new(),
        overflow: false,
        max,
        pool: reservation.pool.clone(),
        key: reservation.key.clone(),
        media,
        status: parts.status.as_u16() as i32,
        finish: None,
        done: false,
    };
    reservation.armed = false;
    Response::from_parts(parts, Body::from_stream(stream))
}
struct Recording {
    source: BodyDataStream,
    buffer: Vec<u8>,
    overflow: bool,
    max: usize,
    pool: PgPool,
    key: RuntimeIdempotency,
    media: String,
    status: i32,
    finish: Option<AgentFuture<'static, ()>>,
    done: bool,
}
impl Stream for Recording {
    type Item = Result<Bytes, AgentError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        if let Some(future) = self.finish.as_mut() {
            return match future.as_mut().poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(result) => {
                    self.done = true;
                    match result {
                        Ok(()) => Poll::Ready(None),
                        Err(e) => Poll::Ready(Some(Err(e))),
                    }
                }
            };
        }
        match Pin::new(&mut self.source).poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(bytes))) => {
                if bytes.len() <= self.max.saturating_sub(self.buffer.len()) && !self.overflow {
                    self.buffer.extend_from_slice(&bytes);
                } else {
                    self.overflow = true;
                    self.buffer.clear();
                }
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(_))) => {
                self.done = true;
                let key = self.key.clone();
                let pool = self.pool.clone();
                tokio::spawn(async move {
                    let _ = key.fail(&pool).await;
                });
                Poll::Ready(Some(Err(AgentError::code(
                    503,
                    "AGENT_RUNTIME_STREAM_INTERRUPTED",
                ))))
            }
            Poll::Ready(None) => {
                let key = self.key.clone();
                let pool = self.pool.clone();
                let bytes = std::mem::take(&mut self.buffer);
                let overflow = self.overflow;
                let media = self.media.clone();
                let status = self.status;
                self.finish = Some(Box::pin(async move {
                    if overflow {
                        key.fail(&pool).await
                    } else {
                        key.complete(&pool, bytes, &media, status).await
                    }
                }));
                self.poll_next(cx)
            }
        }
    }
}
impl Drop for Recording {
    fn drop(&mut self) {
        if !self.done {
            let key = self.key.clone();
            let pool = self.pool.clone();
            tokio::spawn(async move {
                let _ = key.fail(&pool).await;
            });
        }
    }
}
