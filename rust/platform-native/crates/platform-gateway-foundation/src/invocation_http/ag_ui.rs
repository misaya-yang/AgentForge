//! AG-UI 1.0 event stream over the platform Thread/Turn/Item projection.
//! `POST /api/v1/agui/runs` turns canonical assistant-turn events into the
//! SSE stream documented at <https://docs.ag-ui.com/spec/1.0>.
use super::{InvocationIdentity, InvocationState};
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header},
    response::Response,
    routing::post,
};
use serde_json::{Value, json};

const MAX_BODY: usize = 1024 * 1024;

pub(super) fn router(state: InvocationState) -> Router {
    Router::new()
        .route("/api/v1/agui/runs", post(run))
        .with_state(state)
}

async fn run(State(state): State<InvocationState>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let headers = request.headers().clone();
    if let Err(response) = reject_anonymous(&headers) {
        return response;
    }
    if let Err(error) = authorize(&state, &headers, &path).await {
        return error;
    }
    let bytes = match axum::body::to_bytes(request.into_body(), MAX_BODY).await {
        Ok(bytes) => bytes,
        Err(_) => return protocol_error(StatusCode::PAYLOAD_TOO_LARGE, "request body is too large"),
    };
    let body: Value = match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) if value.is_object() => value,
        _ => return protocol_error(StatusCode::BAD_REQUEST, "JSON object body is required"),
    };
    match project_run(&body) {
        Ok(events) => sse_response(&events),
        Err(message) => protocol_error(StatusCode::BAD_REQUEST, message),
    }
}

pub(crate) fn reject_anonymous(headers: &HeaderMap) -> Result<(), Response> {
    let has_authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| !v.trim().is_empty());
    let has_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| !v.trim().is_empty());
    if has_authorization || has_key {
        Ok(())
    } else {
        Err(protocol_error(
            StatusCode::UNAUTHORIZED,
            "authentication required",
        ))
    }
}

async fn authorize(
    state: &InvocationState,
    headers: &HeaderMap,
    path: &str,
) -> Result<InvocationIdentity, Response> {
    super::resolve_identity(&state.auth, state.identity.as_deref(), headers, path)
        .await
        .map_err(|error| *error)
}

/// Project one run. `events` are canonical Thread/Turn/Item records
/// (`event_type` + `data`). A run that omits them still opens and closes so a
/// client can attach; model text then comes from the runtime projection.
pub fn project_run(body: &Value) -> Result<Vec<Value>, &'static str> {
    let thread_id = text(body, "threadId").or_else(|_| text(body, "thread_id"))?;
    let run_id = text(body, "runId").or_else(|_| text(body, "run_id"))?;
    if thread_id.is_empty() || run_id.is_empty() {
        return Err("threadId and runId are required");
    }
    let mut out = vec![json!({
        "type": "RUN_STARTED",
        "threadId": thread_id,
        "runId": run_id,
        "protocolVersion": "1.0",
    })];
    let events = body.get("events").and_then(Value::as_array);
    let mut message_open = false;
    let mut reasoning_open = false;
    let mut saw_error = false;
    let message_id = format!("msg_{run_id}");
    if let Some(events) = events {
        for event in events {
            let kind = event
                .get("event_type")
                .and_then(Value::as_str)
                .unwrap_or("");
            let data = event.get("data").cloned().unwrap_or_else(|| json!({}));
            match kind {
                "text_delta" => {
                    if !message_open {
                        out.push(json!({"type":"TEXT_MESSAGE_START","messageId":message_id,"role":"assistant"}));
                        message_open = true;
                    }
                    let delta = data["content"].as_str().unwrap_or("");
                    out.push(json!({"type":"TEXT_MESSAGE_CONTENT","messageId":message_id,"delta":delta}));
                }
                "thinking_delta" => {
                    if !reasoning_open {
                        out.push(json!({"type":"REASONING_START","messageId":format!("rsn_{run_id}")}));
                        reasoning_open = true;
                    }
                    out.push(json!({
                        "type": "REASONING_MESSAGE_CONTENT",
                        "messageId": format!("rsn_{run_id}"),
                        "delta": data["content"].as_str().unwrap_or(""),
                    }));
                }
                "tool_call_start" => {
                    let call_id = call_id(&data);
                    let name = data["tool_name"].as_str().or_else(|| data["name"].as_str()).unwrap_or("");
                    out.push(json!({"type":"TOOL_CALL_START","toolCallId":call_id,"toolCallName":name}));
                    if let Some(args) = arguments_text(&data) {
                        out.push(json!({"type":"TOOL_CALL_ARGS","toolCallId":call_id,"delta":args}));
                    }
                }
                "tool_call_result" => {
                    let call_id = call_id(&data);
                    out.push(json!({
                        "type": "TOOL_CALL_RESULT",
                        "toolCallId": call_id,
                        "messageId": message_id,
                        "content": data.get("content").cloned().unwrap_or(data.get("output").cloned().unwrap_or(Value::Null)),
                    }));
                }
                "tool_call_end" => {
                    out.push(json!({"type":"TOOL_CALL_END","toolCallId":call_id(&data)}));
                }
                "run_error" => {
                    saw_error = true;
                    out.push(json!({
                        "type": "RUN_ERROR",
                        "threadId": thread_id,
                        "runId": run_id,
                        "message": data["message"].as_str().unwrap_or("run failed"),
                    }));
                }
                "run_finished" | "run_started" | "plan_update" => {}
                _ => {}
            }
        }
    }
    if reasoning_open {
        out.push(json!({"type":"REASONING_END","messageId":format!("rsn_{run_id}")}));
    }
    if message_open {
        out.push(json!({"type":"TEXT_MESSAGE_END","messageId":message_id}));
    }
    if !saw_error {
        out.push(json!({"type":"RUN_FINISHED","threadId":thread_id,"runId":run_id}));
    }
    Ok(out)
}

fn call_id(data: &Value) -> String {
    data.get("tool_call_id")
        .or_else(|| data.get("call_id"))
        .and_then(Value::as_str)
        .unwrap_or("call")
        .to_owned()
}

fn arguments_text(data: &Value) -> Option<String> {
    let value = data.get("arguments")?;
    if value.is_null() {
        return None;
    }
    if let Some(text) = value.as_str() {
        return (!text.is_empty()).then(|| text.to_owned());
    }
    Some(value.to_string())
}

fn text<'a>(body: &'a Value, key: &str) -> Result<&'a str, &'static str> {
    body.get(key)
        .and_then(Value::as_str)
        .ok_or("threadId and runId are required")
}

fn sse_response(events: &[Value]) -> Response {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(&event.to_string());
        body.push_str("\n\n");
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(body))
        .unwrap_or_else(|_| protocol_error(StatusCode::INTERNAL_SERVER_ERROR, "stream unavailable"))
}

fn protocol_error(status: StatusCode, message: &str) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"error":{"message":message,"protocol":"ag-ui","version":"1.0"}}).to_string(),
        ))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_thread_turn_item_into_ag_ui_1() {
        let body = json!({
            "threadId": "thr-1",
            "runId": "run-1",
            "events": [
                {"event_type":"run_started","data":{"thread_id":"thr-1","run_id":"run-1"}},
                {"event_type":"text_delta","data":{"content":"Hello"}},
                {"event_type":"text_delta","data":{"content":" world"}},
                {"event_type":"tool_call_start","data":{"tool_call_id":"call-1","tool_name":"space_read_page","arguments":"{\"item_id\":\"item-1\"}"}},
                {"event_type":"tool_call_result","data":{"tool_call_id":"call-1","content":"page"}},
                {"event_type":"tool_call_end","data":{"tool_call_id":"call-1"}},
                {"event_type":"run_finished","data":{"status":"succeeded"}}
            ]
        });
        let events = project_run(&body).expect("projection");
        assert_eq!(events[0]["type"], "RUN_STARTED");
        assert_eq!(events[0]["protocolVersion"], "1.0");
        assert!(events.iter().any(|e| e["type"] == "TEXT_MESSAGE_START"));
        assert!(events.iter().any(|e| e["type"] == "TEXT_MESSAGE_CONTENT" && e["delta"] == "Hello"));
        assert!(events.iter().any(|e| e["type"] == "TOOL_CALL_START" && e["toolCallName"] == "space_read_page"));
        assert!(events.iter().any(|e| e["type"] == "TOOL_CALL_ARGS"));
        assert!(events.iter().any(|e| e["type"] == "TOOL_CALL_RESULT"));
        assert!(events.iter().any(|e| e["type"] == "TOOL_CALL_END"));
        assert_eq!(events.last().expect("terminal")["type"], "RUN_FINISHED");
        let rendered = sse_response(&events);
        assert_eq!(
            rendered.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );
    }

    #[test]
    fn run_error_replaces_run_finished() {
        let events = project_run(&json!({
            "threadId": "thr",
            "runId": "run",
            "events": [{"event_type":"run_error","data":{"message":"denied"}}]
        }))
        .expect("projection");
        assert!(events.iter().any(|e| e["type"] == "RUN_ERROR" && e["message"] == "denied"));
        assert!(events.iter().all(|e| e["type"] != "RUN_FINISHED"));
    }

    #[test]
    fn anonymous_runs_are_rejected() {
        let error = reject_anonymous(&HeaderMap::new()).expect_err("anonymous");
        assert_eq!(error.status(), StatusCode::UNAUTHORIZED);
    }
}
