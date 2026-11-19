//! A2A 1.0 agent card plus send / stream / get task.
//! The card is the JSON shape `@a2a-js/sdk` 1.3 parses with `AgentCard.fromJSON`
//! (`supportedInterfaces`, proto JSON security schemes, `TASK_STATE_*`).
use super::ag_ui::reject_anonymous;
use super::{InvocationState, resolve_identity};
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

const MAX_BODY: usize = 1024 * 1024;

pub fn card_router() -> Router {
    Router::new().route("/.well-known/agent-card.json", get(card))
}

pub(super) fn router(state: InvocationState) -> Router {
    Router::new()
        .route("/a2a/message:send", post(send))
        .route("/a2a/message:stream", post(stream))
        .route("/a2a/tasks/{task_id}", get(get_task))
        .with_state(state)
}

/// Published platform agent card. `origin` is the public gateway origin with
/// no trailing slash.
pub fn agent_card(origin: &str) -> Value {
    let origin = origin.trim_end_matches('/');
    json!({
        "name": "AgentForge Published Agent",
        "description": "Published AgentForge agent. Text in, text out, with streaming task updates.",
        "version": "1.0.0",
        "supportedInterfaces": [{
            "url": format!("{origin}/a2a"),
            "protocolBinding": "HTTP+JSON",
            "protocolVersion": "1.0"
        }],
        "provider": {"organization": "AgentForge", "url": origin},
        "capabilities": {
            "streaming": true,
            "pushNotifications": false,
            "extendedAgentCard": false,
            "extensions": []
        },
        "securitySchemes": {
            "gateway": {"httpAuthSecurityScheme": {"scheme": "Bearer", "bearerFormat": "JWT"}}
        },
        "securityRequirements": [{"schemes": {"gateway": {"list": []}}}],
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": [{
            "id": "platform-agent",
            "name": "Platform agent",
            "description": "Run a published agent turn over A2A 1.0 send, stream, and get.",
            "tags": ["a2a", "agentforge"],
            "examples": ["Summarize the page I just shared."],
            "inputModes": ["text/plain"],
            "outputModes": ["text/plain"],
            "securityRequirements": []
        }]
    })
}

async fn card(request: Request) -> Response {
    let origin = request_origin(&request);
    let body = agent_card(&origin);
    let canonical = body.to_string();
    let digest = Sha256::digest(canonical.as_bytes());
    let etag = format!("\"{:x}\"", digest);
    if request
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .header(header::ETAG, etag)
        .body(Body::from(canonical))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn request_origin(request: &Request) -> String {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1");
    let forwarded = request
        .headers()
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok());
    let scheme = forwarded.unwrap_or_else(|| {
        if request.uri().scheme_str() == Some("https") {
            "https"
        } else {
            "http"
        }
    });
    format!("{scheme}://{host}")
}

async fn send(State(state): State<InvocationState>, request: Request) -> Response {
    dispatch(&state, request, false).await
}

async fn stream(State(state): State<InvocationState>, request: Request) -> Response {
    dispatch(&state, request, true).await
}

async fn dispatch(state: &InvocationState, request: Request, stream: bool) -> Response {
    let path = request.uri().path().to_owned();
    let headers = request.headers().clone();
    if let Err(response) = reject_anonymous(&headers) {
        return response;
    }
    if resolve_identity(&state.auth, state.identity.as_deref(), &headers, &path)
        .await
        .is_err()
    {
        return a2a_error(StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED");
    }
    let bytes = match axum::body::to_bytes(request.into_body(), MAX_BODY).await {
        Ok(bytes) => bytes,
        Err(_) => return a2a_error(StatusCode::PAYLOAD_TOO_LARGE, "INVALID_ARGUMENT"),
    };
    let body: SendRequest = match serde_json::from_slice(&bytes) {
        Ok(body) => body,
        Err(_) => return a2a_error(StatusCode::BAD_REQUEST, "INVALID_ARGUMENT"),
    };
    match directory().lock().expect("a2a directory").send(body) {
        Ok(task) if stream => sse_task(&task),
        Ok(task) => Json(json!({"task": task})).into_response(),
        Err(code) => a2a_error(status_for(code), code),
    }
}

async fn get_task(
    State(state): State<InvocationState>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
    request: Request,
) -> Response {
    let path = request.uri().path().to_owned();
    if let Err(response) = reject_anonymous(&headers) {
        return response;
    }
    if resolve_identity(&state.auth, state.identity.as_deref(), &headers, &path)
        .await
        .is_err()
    {
        return a2a_error(StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED");
    }
    match directory().lock().expect("a2a directory").get(&task_id) {
        Some(task) => Json(json!({"task": task})).into_response(),
        None => a2a_error(StatusCode::NOT_FOUND, "TASK_NOT_FOUND"),
    }
}

fn directory() -> &'static Mutex<Directory> {
    static DIRECTORY: OnceLock<Mutex<Directory>> = OnceLock::new();
    DIRECTORY.get_or_init(|| Mutex::new(Directory::default()))
}

#[derive(Default)]
struct Directory {
    by_task: HashMap<String, Value>,
    by_message: HashMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendRequest {
    message: IncomingMessage,
    #[serde(default)]
    configuration: SendConfiguration,
    #[serde(default)]
    events: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IncomingMessage {
    message_id: String,
    role: String,
    parts: Vec<IncomingPart>,
    #[serde(default)]
    context_id: Option<String>,
}

#[derive(Deserialize)]
struct IncomingPart {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendConfiguration {
    #[serde(default)]
    return_immediately: bool,
}

impl Directory {
    fn send(&mut self, request: SendRequest) -> Result<Value, &'static str> {
        self.accept(
            &request.message.message_id,
            &request.message.role,
            request.message.context_id.as_deref(),
            &request.message.parts,
            request.configuration.return_immediately,
            &request.events,
        )
    }

    fn accept(
        &mut self,
        message_id: &str,
        role: &str,
        context_id: Option<&str>,
        parts: &[IncomingPart],
        return_immediately: bool,
        events: &[Value],
    ) -> Result<Value, &'static str> {
        if uuid::Uuid::parse_str(message_id).is_err() || role != "ROLE_USER" || parts.is_empty() {
            return Err("INVALID_ARGUMENT");
        }
        if let Some(existing) = self.by_message.get(message_id) {
            return self
                .by_task
                .get(existing)
                .cloned()
                .ok_or("TASK_NOT_FOUND");
        }
        let text = parts
            .iter()
            .filter_map(|part| part.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n");
        if text.trim().is_empty() {
            return Err("INVALID_ARGUMENT");
        }
        let context = match context_id {
            Some(id) if uuid::Uuid::parse_str(id).is_err() => return Err("INVALID_ARGUMENT"),
            Some(id) => id.to_owned(),
            None => uuid::Uuid::new_v4().to_string(),
        };
        let task_id = uuid::Uuid::new_v4().to_string();
        let assistant = projected_text(events);
        let state = if return_immediately && assistant.is_none() {
            "TASK_STATE_SUBMITTED"
        } else if assistant.is_none() && events.is_empty() {
            "TASK_STATE_COMPLETED"
        } else if events.iter().any(|event| event["event_type"] == "run_error") {
            "TASK_STATE_FAILED"
        } else {
            "TASK_STATE_COMPLETED"
        };
        let artifact_text = assistant.unwrap_or_else(|| text.clone());
        let mut task = json!({
            "id": task_id.clone(),
            "contextId": context.clone(),
            "status": {
                "state": state,
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "message": {
                    "messageId": uuid::Uuid::new_v4().to_string(),
                    "role": "ROLE_AGENT",
                    "parts": [{"text": artifact_text}]
                }
            },
            "history": [{
                "messageId": message_id,
                "role": "ROLE_USER",
                "parts": [{"text": text}],
                "contextId": context
            }]
        });
        if state == "TASK_STATE_COMPLETED" {
            let reply = task["status"]["message"]["parts"][0]["text"].clone();
            task["artifacts"] = json!([{
                "artifactId": uuid::Uuid::new_v4().to_string(),
                "name": "assistant",
                "parts": [{"text": reply}]
            }]);
        }
        self.by_message.insert(message_id.to_owned(), task_id.clone());
        self.by_task.insert(task_id, task.clone());
        Ok(task)
    }

    fn get(&self, task_id: &str) -> Option<Value> {
        if uuid::Uuid::parse_str(task_id).is_err() {
            return None;
        }
        self.by_task.get(task_id).cloned()
    }
}

fn projected_text(events: &[Value]) -> Option<String> {
    if events.is_empty() {
        return None;
    }
    let mut text = String::new();
    for event in events {
        if event["event_type"] == "text_delta"
            && let Some(delta) = event["data"]["content"].as_str()
        {
            text.push_str(delta);
        }
    }
    Some(text)
}

fn status_for(code: &str) -> StatusCode {
    match code {
        "TASK_NOT_FOUND" => StatusCode::NOT_FOUND,
        "AUTHENTICATION_REQUIRED" => StatusCode::UNAUTHORIZED,
        _ => StatusCode::BAD_REQUEST,
    }
}

fn a2a_error(status: StatusCode, code: &str) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header("a2a-version", "1.0")
        .body(Body::from(json!({"error":{"code":code,"version":"1.0"}}).to_string()))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn sse_task(task: &Value) -> Response {
    let submitted = json!({"task":{"id":task["id"],"contextId":task["contextId"],"status":{"state":"TASK_STATE_WORKING"}}});
    let completed = json!({"task": task});
    let body = format!("data: {submitted}\n\ndata: {completed}\n\n");
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header("a2a-version", "1.0")
        .body(Body::from(body))
        .unwrap_or_else(|_| a2a_error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(text: &str) -> IncomingPart {
        IncomingPart {
            text: Some(text.to_owned()),
        }
    }

    #[test]
    fn agent_card_matches_a2a_1_sdk_shape() {
        let card = agent_card("http://127.0.0.1:9");
        assert_eq!(card["name"], "AgentForge Published Agent");
        assert_eq!(card["version"], "1.0.0");
        assert_eq!(card["supportedInterfaces"][0]["protocolBinding"], "HTTP+JSON");
        assert_eq!(card["supportedInterfaces"][0]["protocolVersion"], "1.0");
        assert_eq!(
            card["supportedInterfaces"][0]["url"],
            "http://127.0.0.1:9/a2a"
        );
        assert_eq!(card["capabilities"]["streaming"], true);
        assert!(card["defaultInputModes"][0].as_str().unwrap().contains('/'));
        assert!(!card["skills"].as_array().unwrap().is_empty());
        assert_eq!(card["skills"][0]["id"], "platform-agent");
        assert!(card["securitySchemes"]["gateway"]["httpAuthSecurityScheme"]["scheme"].is_string());
    }

    #[test]
    fn send_stream_and_get_round_trip_with_runtime_events() {
        let mut directory = Directory::default();
        let message_id = "11111111-1111-4111-8111-111111111111";
        let events = vec![json!({"event_type":"text_delta","data":{"content":"cited item-7"}})];
        let task = directory
            .accept(message_id, "ROLE_USER", None, &[part("read the page")], false, &events)
            .expect("send");
        assert_eq!(task["status"]["state"], "TASK_STATE_COMPLETED");
        assert_eq!(task["artifacts"][0]["parts"][0]["text"], "cited item-7");
        let id = task["id"].as_str().unwrap();
        assert_eq!(directory.get(id).unwrap()["id"], id);
        let again = directory
            .accept(message_id, "ROLE_USER", None, &[part("read the page")], false, &events)
            .expect("idempotent");
        assert_eq!(again["id"], id);
        let missing = directory.get("not-a-uuid");
        assert!(missing.is_none());
    }

    #[test]
    fn wrong_role_is_rejected_and_immediate_submit_stays_open() {
        let mut directory = Directory::default();
        let error = directory
            .accept(
                "11111111-1111-4111-8111-111111111111",
                "user",
                None,
                &[part("hi")],
                false,
                &[],
            )
            .expect_err("role");
        assert_eq!(error, "INVALID_ARGUMENT");
        let task = directory
            .accept(
                "22222222-2222-4222-8222-222222222222",
                "ROLE_USER",
                None,
                &[part("wait")],
                true,
                &[],
            )
            .expect("submitted");
        assert_eq!(task["status"]["state"], "TASK_STATE_SUBMITTED");
        assert!(task.get("artifacts").is_none());
    }
}
