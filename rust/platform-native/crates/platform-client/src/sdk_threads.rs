//! Stable SDK projection of V2 rollout items, independent from desktop projection.
use crate::{Client, Error, sdk_stream::SdkEvent};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
pub fn project_v2_event(event: SdkEvent) -> Option<SdkEvent> {
    if event.event_type != "item" || !event.data.is_object() {
        return Some(event);
    }
    let envelope = event
        .data
        .get("event")
        .filter(|v| v.is_object())
        .unwrap_or(&event.data);
    let empty = json!({});
    let raw = envelope
        .get("payload")
        .filter(|v| v.is_object())
        .unwrap_or(&empty);
    let data = raw.get("data").unwrap_or(&Value::Null);
    if let Some(kind) = raw["event_type"]
        .as_str()
        .filter(|s| !["item", "rollout/item"].contains(s))
    {
        return Some(SdkEvent {
            event_type: kind.into(),
            data: if data.is_object() {
                data.clone()
            } else {
                json!({"value":data})
            },
            timestamp: event.timestamp,
        });
    }
    let item = if data.is_object() { data } else { raw };
    let payload = item
        .get("payload")
        .filter(|v| v.is_object())
        .unwrap_or(item);
    let kind = item["type"]
        .as_str()
        .or(payload["type"].as_str())
        .unwrap_or_default();
    let content = match &payload["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| {
                p.get("text")
                    .or_else(|| p.get("content"))
                    .and_then(Value::as_str)
            })
            .collect::<String>(),
        _ => String::new(),
    };
    let text = if content.is_empty() {
        payload["message"]
            .as_str()
            .or(payload["text"].as_str())
            .unwrap_or_default()
            .into()
    } else {
        content
    };
    let output = |kind: &str, data: Value| {
        Some(SdkEvent {
            event_type: kind.into(),
            data,
            timestamp: event.timestamp,
        })
    };
    if !text.is_empty() && (payload["role"] == "assistant" || payload["type"] == "agent_message") {
        return output("text_delta", json!({"content":text}));
    }
    if !text.is_empty() && (payload["role"] == "reasoning" || payload["type"] == "reasoning") {
        return output("thinking_delta", json!({"content":text}));
    }
    if payload.get("approval_id").is_some_and(|v| !v.is_null()) || kind == "approval_request" {
        return output("approval_required", payload.clone());
    }
    if payload.get("artifact_id").is_some_and(|v| !v.is_null()) || kind == "artifact" {
        return output("artifact_created", payload.clone());
    }
    let tool = payload.get("name").or_else(|| payload.get("tool"));
    if tool.is_some()
        || [
            "function_call",
            "tool_use",
            "command_execution",
            "mcp_tool_call",
        ]
        .contains(&kind)
    {
        let status = item["status"]
            .as_str()
            .or(payload["status"].as_str())
            .unwrap_or_default()
            .to_lowercase();
        return output(
            if ["completed", "succeeded", "failed", "error", "cancelled"].contains(&status.as_str())
            {
                "tool_call_result"
            } else {
                "tool_call_start"
            },
            json!({"tool_call_id":item.get("id").or_else(||payload.get("id")),"tool_name":tool.cloned().unwrap_or(json!(kind)),"arguments":payload.get("arguments").or_else(||payload.get("input")),"result":payload.get("result").or_else(||payload.get("output")),"status":item.get("status").or_else(||payload.get("status"))}),
        );
    }
    if ["activity", "event_msg"].contains(&kind) {
        return output("activity", payload.clone());
    }
    None
}
impl Client {
    pub async fn sdk_thread_events(
        &self,
        thread: &str,
        turn: Option<&Value>,
        turn_id: Option<&str>,
        after_sequence: u64,
        limit: u32,
        sender: mpsc::Sender<Result<SdkEvent, Error>>,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<(), Error> {
        let returned = turn
            .and_then(|v| v.get("turn").unwrap_or(v).get("events_url"))
            .and_then(Value::as_str);
        let mut url = if let Some(relative) = returned {
            self.events_url(thread, relative)?
        } else {
            self.endpoint(&["api", "v2", "agent", "threads", thread, "events"])?
        };
        if returned.is_none() {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("after_sequence", &after_sequence.to_string())
                .append_pair("limit", &limit.to_string());
            if let Some(id) = turn_id {
                query.append_pair("turn_id", id);
            }
        }
        if *cancel.borrow() || sender.is_closed() {
            return Ok(());
        }
        let http = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| Error::Transport)?;
        let pending = self.authorize(http.get(url).header("Accept", "text/event-stream"))?;
        let response = tokio::select! {r=tokio::time::timeout(self.stream_limits.header_timeout(),pending.send())=>r.map_err(|_|Error::Transport)?.map_err(|_|Error::Transport)?,_ = cancel.changed()=>return Ok(()),_ = sender.closed()=>return Ok(())};
        crate::sdk_stream::receive(
            response,
            sender,
            cancel,
            project_v2_event,
            self.stream_limits,
        )
        .await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk_stream::SdkDecoder;
    #[test]
    fn shared_item_fixture_projects_text() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../../sdk/fixtures/sse_inner_envelopes.json"
        ))
        .unwrap();
        let mut decoder = SdkDecoder::default();
        let raw = decoder
            .push(fixture["agent_v2_item"]["sse"].as_str().unwrap().as_bytes())
            .unwrap()
            .remove(0);
        let projected = project_v2_event(raw).unwrap();
        assert_eq!(projected.event_type, "text_delta");
        assert_eq!(projected.text(), "hello");
    }
    #[test]
    fn tool_completion_and_unrecognized_items() {
        let e = SdkEvent {
            event_type: "item".into(),
            data: json!({"event":{"payload":{"type":"function_call","id":"c","status":"completed","payload":{"name":"tool","output":"ok"}}}}),
            timestamp: None,
        };
        let p = project_v2_event(e).unwrap();
        assert_eq!(p.event_type, "tool_call_result");
        assert_eq!(p.data["tool_call_id"], "c");
    }
}
