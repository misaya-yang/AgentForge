//! Vercel AI SDK UI Message Stream (`ai` 7.x) over the same Thread/Turn/Item
//! events. `POST /api/v1/ai-sdk/chat` speaks the UI message stream protocol:
//! `data: {json}` frames terminated by `data: [DONE]`.
use super::ag_ui::reject_anonymous;
use super::{InvocationState, resolve_identity};
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{StatusCode, header},
    response::Response,
    routing::post,
};
use serde_json::{Value, json};

const MAX_BODY: usize = 1024 * 1024;

pub(super) fn router(state: InvocationState) -> Router {
    Router::new()
        .route("/api/v1/ai-sdk/chat", post(chat))
        .with_state(state)
}

async fn chat(State(state): State<InvocationState>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let headers = request.headers().clone();
    if let Err(response) = reject_anonymous(&headers) {
        return response;
    }
    if resolve_identity(&state.auth, state.identity.as_deref(), &headers, &path)
        .await
        .is_err()
    {
        return json_error(StatusCode::UNAUTHORIZED, "authentication required");
    }
    let bytes = match axum::body::to_bytes(request.into_body(), MAX_BODY).await {
        Ok(bytes) => bytes,
        Err(_) => return json_error(StatusCode::PAYLOAD_TOO_LARGE, "request body is too large"),
    };
    let body: Value = match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) if value.is_object() => value,
        _ => return json_error(StatusCode::BAD_REQUEST, "JSON object body is required"),
    };
    match project_chat(&body) {
        Ok(frames) => {
            let mut body = String::new();
            for frame in frames {
                body.push_str("data: ");
                body.push_str(&frame);
                body.push_str("\n\n");
            }
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .header("x-vercel-ai-ui-message-stream", "v1")
                .body(Body::from(body))
                .unwrap_or_else(|_| json_error(StatusCode::INTERNAL_SERVER_ERROR, "stream unavailable"))
        }
        Err(message) => json_error(StatusCode::BAD_REQUEST, message),
    }
}

/// UI message stream frames, including the terminal `[DONE]` sentinel.
pub fn project_chat(body: &Value) -> Result<Vec<String>, &'static str> {
    let message_id = body
        .get("id")
        .or_else(|| body.get("messageId"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .unwrap_or("msg_ui");
    let events = body
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut frames = vec![
        json!({"type":"start","messageId":message_id}).to_string(),
        json!({"type":"start-step"}).to_string(),
    ];
    let mut text_open = false;
    let text_id = format!("txt_{message_id}");
    let mut failed = false;
    for event in events {
        let kind = event.get("event_type").and_then(Value::as_str).unwrap_or("");
        let data = event.get("data").cloned().unwrap_or_else(|| json!({}));
        match kind {
            "text_delta" => {
                if !text_open {
                    frames.push(json!({"type":"text-start","id":text_id}).to_string());
                    text_open = true;
                }
                frames.push(
                    json!({"type":"text-delta","id":text_id,"delta":data["content"].as_str().unwrap_or("")})
                        .to_string(),
                );
            }
            "tool_call_start" => {
                let call_id = data
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("call");
                let name = data
                    .get("tool_name")
                    .or_else(|| data.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                frames.push(
                    json!({"type":"tool-input-start","toolCallId":call_id,"toolName":name}).to_string(),
                );
                if let Some(arguments) = data.get("arguments") {
                    let delta = if arguments.is_string() {
                        arguments.as_str().unwrap_or("").to_owned()
                    } else {
                        arguments.to_string()
                    };
                    if !delta.is_empty() {
                        frames.push(
                            json!({"type":"tool-input-delta","toolCallId":call_id,"inputTextDelta":delta}).to_string(),
                        );
                    }
                    let input = arguments
                        .as_str()
                        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                        .filter(Value::is_object)
                        .unwrap_or_else(|| {
                            if arguments.is_object() {
                                arguments.clone()
                            } else {
                                json!({})
                            }
                        });
                    frames.push(
                        json!({"type":"tool-input-available","toolCallId":call_id,"toolName":name,"input":input})
                            .to_string(),
                    );
                }
            }
            "tool_call_result" => {
                let call_id = data
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("call");
                let output = data
                    .get("content")
                    .cloned()
                    .or_else(|| data.get("output").cloned())
                    .unwrap_or(Value::Null);
                frames.push(
                    json!({"type":"tool-output-available","toolCallId":call_id,"output":output}).to_string(),
                );
            }
            "run_error" => {
                failed = true;
                frames.push(
                    json!({"type":"error","errorText":data["message"].as_str().unwrap_or("run failed")})
                        .to_string(),
                );
            }
            _ => {}
        }
    }
    if text_open {
        frames.push(json!({"type":"text-end","id":text_id}).to_string());
    }
    frames.push(json!({"type":"finish-step"}).to_string());
    frames.push(
        json!({"type":"finish","finishReason":if failed{"error"}else{"stop"}}).to_string(),
    );
    frames.push("[DONE]".to_owned());
    Ok(frames)
}

fn json_error(status: StatusCode, message: &str) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"error":{"message":message,"protocol":"ai-sdk-ui-message-stream"}}).to_string(),
        ))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_ui_message_stream_with_tool_and_done() {
        let frames = project_chat(&json!({
            "id": "msg-1",
            "events": [
                {"event_type":"text_delta","data":{"content":"ok"}},
                {"event_type":"tool_call_start","data":{"tool_call_id":"c1","tool_name":"space_search","arguments":"{\"q\":\"policy\"}"}},
                {"event_type":"tool_call_result","data":{"tool_call_id":"c1","content":"hit"}},
                {"event_type":"run_finished","data":{}}
            ]
        }))
        .expect("frames");
        assert_eq!(frames[0], json!({"type":"start","messageId":"msg-1"}).to_string());
        assert!(frames.iter().any(|f| f.contains("\"text-delta\"") && f.contains("ok")));
        assert!(frames.iter().any(|f| f.contains("\"tool-input-available\"") && f.contains("space_search")));
        assert!(frames.iter().any(|f| f.contains("\"tool-output-available\"")));
        assert!(frames.iter().any(|f| f.contains("\"finishReason\":\"stop\"")));
        assert_eq!(frames.last().map(String::as_str), Some("[DONE]"));
    }
}
