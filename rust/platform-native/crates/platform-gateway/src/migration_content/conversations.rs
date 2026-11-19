//! Business-friendly conversation facade over the canonical LangGraph adapter.
use super::{ContentAccess, ContentError, ContentIdentity, string};
use crate::guest_session_manager::GuestSessionManager;
use crate::migration_compat::{
    common::{CompatError, CompatUser, SseDecoder, sse},
    langgraph::LangGraphClient,
};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_core::Stream;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
// Both domain errors are now owned by extracted crates. Keep this endpoint's
// historical redaction/conversation-404 mapping explicit at its adapter boundary.
fn conversation_error(error: CompatError) -> ContentError {
    ContentError::new(
        error.status,
        if error.status == 404 {
            json!("Conversation not found")
        } else {
            error.detail
        },
    )
}
fn compat(u: &ContentIdentity, headers: &HeaderMap) -> CompatUser {
    CompatUser {
        user_id: u.user_id.clone(),
        tenant_id: u.tenant_id.clone(),
        tier: u.tier.clone(),
        authenticated: u.is_authenticated,
        roles: u.roles.clone(),
        permissions: u.permissions.clone(),
        request_id: headers
            .get("x-request-id")
            .and_then(|s| s.to_str().ok())
            .unwrap_or("")
            .into(),
        trace_id: headers
            .get("x-trace-id")
            .and_then(|s| s.to_str().ok())
            .unwrap_or("")
            .into(),
    }
}
pub fn thread_to_conversation(t: &Value) -> Value {
    let mut meta = t
        .get("metadata")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let title = meta.get("title").cloned().unwrap_or(Value::Null);
    let created = meta
        .get("created_at")
        .cloned()
        .unwrap_or(t["created_at"].clone());
    for k in ["owner_id", "tenant_id", "user_tier", "created_at", "title"] {
        meta.remove(k);
    }
    json!({"conversation_id":t["thread_id"],"title":title,"created_at":created,"updated_at":t["updated_at"],"status":t.get("status").cloned().unwrap_or(json!("active")),"metadata":meta})
}
pub fn history_to_messages(history: &[Value]) -> Vec<Value> {
    let mut result = vec![];
    for item in history {
        if let Some(messages) = item["values"]["messages"].as_array() {
            for m in messages {
                result.push(json!({"message_id":m.get("id").cloned().unwrap_or(json!("")),"role":m.get("type").cloned().unwrap_or(json!("unknown")),"content":m.get("content").cloned().unwrap_or(json!("")),"created_at":item.get("created_at").cloned().unwrap_or(json!("")),"metadata":m.get("metadata").cloned().unwrap_or(json!({}))}));
            }
        }
    }
    result
}
#[derive(Clone)]
pub struct ConversationApi {
    pub access: Arc<dyn ContentAccess>,
    pub langgraph: Arc<LangGraphClient>,
    pub guests: Arc<GuestSessionManager>,
}
pub fn router(api: Arc<ConversationApi>) -> Router {
    Router::new()
        .route("/conversations", post(create).get(list))
        .route("/conversations/guest/session", post(guest))
        .route("/conversations/{id}", get(read).delete(delete))
        .route("/conversations/{id}/messages", post(send).get(history))
        .route("/conversations/{id}/stream", post(stream))
        .with_state(api)
}
async fn create(
    State(a): State<Arc<ConversationApi>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ContentError> {
    let u = a.access.resolve(&headers).await?;
    let assistant = body
        .get("assistant_id")
        .and_then(Value::as_str)
        .unwrap_or("agent");
    a.access
        .rate_limit(&u, "thread_create", Some(assistant))
        .await?;
    let mut meta = body
        .get("metadata")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or(json!({}));
    if let Some(title) = body
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        meta["title"] = json!(title)
    }
    meta["assistant_id"] = json!(assistant);
    let thread = a
        .langgraph
        .create_thread(&compat(&u, &headers), &json!({"metadata":meta}))
        .await
        .map_err(conversation_error)?;
    Ok(Json(thread_to_conversation(&thread)))
}
fn integer(
    q: &HashMap<String, String>,
    key: &str,
    default: i64,
    min: i64,
    max: i64,
) -> Result<i64, ContentError> {
    let n = q
        .get(key)
        .map(|s| s.parse::<i64>())
        .transpose()
        .map_err(|_| ContentError::new(422, "Invalid pagination"))?
        .unwrap_or(default);
    if n < min || n > max {
        Err(ContentError::new(422, "Invalid pagination"))
    } else {
        Ok(n)
    }
}
async fn list(
    State(a): State<Arc<ConversationApi>>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = a.access.resolve(&headers).await?;
    let limit = integer(&q, "limit", 10, 1, 100)?;
    let offset = integer(&q, "offset", 0, 0, i64::MAX)?;
    let mut meta = json!({});
    if let Some(status) = q.get("status").filter(|s| !s.is_empty()) {
        meta["status"] = json!(status)
    }
    let rows = a
        .langgraph
        .search_threads(&compat(&u, &headers), meta, limit, offset)
        .await
        .map_err(conversation_error)?;
    Ok(Json(
        json!({"conversations":rows.iter().map(thread_to_conversation).collect::<Vec<_>>(),"total":rows.len(),"limit":limit,"offset":offset}),
    ))
}
async fn read(
    State(a): State<Arc<ConversationApi>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = a.access.resolve(&headers).await?;
    Ok(Json(thread_to_conversation(
        &a.langgraph
            .get_thread(&compat(&u, &headers), &id)
            .await
            .map_err(conversation_error)?,
    )))
}
async fn delete(
    State(a): State<Arc<ConversationApi>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = a.access.resolve(&headers).await?;
    a.langgraph
        .delete_thread(&compat(&u, &headers), &id)
        .await
        .map_err(conversation_error)?;
    Ok(Json(
        json!({"status":"ok","message":"Conversation deleted"}),
    ))
}
async fn send(
    State(a): State<Arc<ConversationApi>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ContentError> {
    let u = a.access.resolve(&headers).await?;
    a.access
        .rate_limit(&u, "run_create", Some("assistant"))
        .await?;
    let content = super::required(&body, "content")?;
    let actor = compat(&u, &headers);
    let thread = a
        .langgraph
        .get_thread(&actor, &id)
        .await
        .map_err(conversation_error)?;
    let assistant = thread["metadata"]
        .get("assistant_id")
        .and_then(Value::as_str)
        .unwrap_or("agent");
    let input = json!({"messages":[{"role":body.get("role").cloned().unwrap_or(json!("user")),"content":content}]});
    let result = a
        .langgraph
        .run_wait(actor, &id, assistant, input, body.get("metadata").cloned())
        .await
        .map_err(conversation_error)?;
    let messages:Vec<_>=result["messages"].as_array().map(|m|m.iter().map(|m|json!({"message_id":m.get("id").cloned().unwrap_or(json!("")),"role":m.get("type").cloned().unwrap_or(json!("assistant")),"content":m.get("content").cloned().unwrap_or(json!(""))})).collect()).unwrap_or_default();
    Ok(Json(
        json!({"conversation_id":id,"messages":messages,"status":result.get("status").cloned().unwrap_or(json!("completed"))}),
    ))
}
async fn history(
    State(a): State<Arc<ConversationApi>>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = a.access.resolve(&headers).await?;
    let limit = integer(&q, "limit", 50, 1, 200)?;
    let messages = history_to_messages(
        &a.langgraph
            .thread_history(
                &compat(&u, &headers),
                &id,
                limit,
                q.get("before").map(String::as_str),
            )
            .await
            .map_err(conversation_error)?,
    );
    Ok(Json(
        json!({"conversation_id":id,"total":messages.len(),"messages":messages}),
    ))
}
struct ConversationStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, axum::Error>> + Send>>,
    decoder: SseDecoder,
    pending: VecDeque<Bytes>,
    ended: bool,
}
impl Stream for ConversationStream {
    type Item = Result<Bytes, std::io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if let Some(bytes) = self.pending.pop_front() {
                return Poll::Ready(Some(Ok(bytes)));
            }
            if self.ended {
                return Poll::Ready(None);
            }
            match self.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(bytes))) => {
                    for (event, data) in self.decoder.push(&bytes) {
                        let event = if matches!(event.as_str(), "messages" | "updates") {
                            "message"
                        } else if event.is_empty() {
                            "message"
                        } else {
                            &event
                        };
                        match serde_json::from_str::<Value>(&data) {
                            Ok(data) => self.pending.push_back(sse(event, &data)),
                            Err(_) => self.pending.push_back(sse(
                                "error",
                                &json!({"error":"Invalid upstream event"}),
                            )),
                        }
                    }
                }
                Poll::Ready(Some(Err(_))) => {
                    self.ended = true;
                    return Poll::Ready(Some(Ok(sse(
                        "error",
                        &json!({"error":"Conversation stream failed"}),
                    ))));
                }
                Poll::Ready(None) => {
                    self.ended = true;
                    return Poll::Ready(Some(Ok(sse("done", &json!({"status":"completed"})))));
                }
            }
        }
    }
}
async fn stream(
    State(a): State<Arc<ConversationApi>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ContentError> {
    let u = a.access.resolve(&headers).await?;
    a.access
        .rate_limit(&u, "run_create", Some("assistant"))
        .await?;
    let content = super::required(&body, "content")?;
    let actor = compat(&u, &headers);
    let thread = a
        .langgraph
        .get_thread(&actor, &id)
        .await
        .map_err(conversation_error)?;
    let assistant = thread["metadata"]
        .get("assistant_id")
        .cloned()
        .unwrap_or(json!("agent"));
    let upstream=a.langgraph.run(actor,&format!("/threads/{id}/runs/stream"),json!({"assistant_id":assistant,"input":{"messages":[{"role":body.get("role").cloned().unwrap_or(json!("user")),"content":content}]},"stream_mode":body.get("stream_mode").cloned().unwrap_or(json!(["messages","updates"]))}),true).await.map_err(conversation_error)?;
    let (status, body) = (upstream.status(), upstream.into_body());
    if !status.is_success() {
        return Ok((status, body).into_response());
    }
    let stream = ConversationStream {
        inner: Box::pin(body.into_data_stream()),
        decoder: SseDecoder::default(),
        pending: VecDeque::new(),
        ended: false,
    };
    Ok((
        [
            ("content-type", "text/event-stream"),
            ("cache-control", "no-cache"),
            ("x-accel-buffering", "no"),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}
async fn guest(State(a): State<Arc<ConversationApi>>) -> Result<Json<Value>, ContentError> {
    let session = a
        .guests
        .create_session(None)
        .await
        .map_err(|_| ContentError::new(503, "Guest session unavailable"))?;
    Ok(Json(
        json!({"session_id":session.session_id,"expires_in":a.guests.config.session_ttl,"message":"Use this session_id in X-Guest-Session header for subsequent requests"}),
    ))
}

#[cfg(test)]
mod extracted_error_tests {
    use super::*;
    #[test]
    fn conversation_adapter_preserves_not_found_redaction_and_other_error_payloads() {
        let hidden =
            conversation_error(CompatError::value(404, json!({"private_thread":"hidden"})));
        assert_eq!(hidden.status, 404);
        assert_eq!(hidden.detail, "Conversation not found");
        for status in [401, 403, 409, 422, 503, 504] {
            let detail = json!({"code":"original_contract","message":"upstream failed"});
            let mapped = conversation_error(CompatError::value(status, detail.clone()));
            assert_eq!(mapped.status, status);
            assert_eq!(mapped.detail, detail);
        }
    }
}
