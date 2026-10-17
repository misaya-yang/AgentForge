//! Provider SSE validation and Chat Completions -> Responses projection.
use super::{AgentError, model::estimate_tokens, text};
use axum::body::Bytes;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
fn error() -> AgentError {
    AgentError::code(502, "RUNTIME_PROVIDER_STREAM_INVALID")
}
pub fn frame(kind: &str, event: &Value) -> Bytes {
    Bytes::from(format!(
        "event: {kind}\ndata: {}\n\n",
        serde_json::to_string(event).expect("JSON event")
    ))
}
pub struct Terminal {
    pub frame: Bytes,
    pub raw_usage: Value,
    pub input: i64,
    pub output: i64,
    pub provider_request_id: Option<String>,
}
pub struct NativeValidator {
    last: i64,
    created: bool,
    terminal: Option<Terminal>,
    aliases: BTreeMap<String, (String, String)>,
    allowed: BTreeSet<(Option<String>, String)>,
    search: bool,
    reasoning: bool,
    arguments: BTreeMap<String, usize>,
    argument_bytes: usize,
}
impl NativeValidator {
    pub fn new(
        aliases: BTreeMap<String, (String, String)>,
        allowed: BTreeSet<(Option<String>, String)>,
        search: bool,
        reasoning: bool,
    ) -> Self {
        Self {
            last: -1,
            created: false,
            terminal: None,
            aliases,
            allowed,
            search,
            reasoning,
            arguments: BTreeMap::new(),
            argument_bytes: 0,
        }
    }
    fn validate_item(&self, item: &mut Value) -> Result<(), AgentError> {
        if !item.is_object() {
            return Ok(());
        }
        match text(item, "type") {
            "function_call" => {
                if let Some((namespace, name)) = self.aliases.get(text(item, "name")) {
                    if item["namespace"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty() && s != namespace)
                    {
                        return Err(AgentError::code(
                            502,
                            "RUNTIME_PROVIDER_TOOL_SCOPE_MISMATCH",
                        ));
                    }
                    item["name"] = name.clone().into();
                    item["namespace"] = namespace.clone().into();
                }
                let identity = (
                    item["namespace"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned),
                    text(item, "name").to_owned(),
                );
                if !self.allowed.contains(&identity) {
                    return Err(AgentError::code(
                        502,
                        "RUNTIME_PROVIDER_TOOL_SCOPE_MISMATCH",
                    ));
                }
                if item["arguments"]
                    .as_str()
                    .is_some_and(|s| s.len() > 1024 * 1024)
                {
                    return Err(AgentError::code(
                        502,
                        "RUNTIME_PROVIDER_TOOL_ARGUMENTS_LIMIT",
                    ));
                }
            }
            "web_search_call" => {
                if !self.search {
                    return Err(AgentError::code(
                        502,
                        "RUNTIME_PROVIDER_TOOL_SCOPE_MISMATCH",
                    ));
                }
            }
            "message" | "reasoning" | "" => {}
            _ => return Err(AgentError::code(502, "RUNTIME_TOOLS_NOT_ENABLED_FOR_PHASE")),
        }
        Ok(())
    }
    pub fn consume(&mut self, payload: &str) -> Result<Option<Bytes>, AgentError> {
        if payload == "[DONE]" {
            return Ok(None);
        }
        let mut event: Value = serde_json::from_str(payload).map_err(|_| error())?;
        if !event.is_object() {
            return Err(error());
        }
        let mut kind = text(&event, "type").to_owned();
        if self.reasoning
            && matches!(
                kind.as_str(),
                "response.reasoning_text.delta" | "response.reasoning_text.done"
            )
        {
            kind = kind.replace("response.reasoning_text", "response.reasoning_summary_text");
            event["type"] = kind.clone().into();
            if event.get("summary_index").is_none() {
                event["summary_index"] = 0.into();
            }
        }
        let seq = event["sequence_number"].as_i64().ok_or_else(error)?;
        if kind.is_empty() || seq != self.last + 1 || self.terminal.is_some() {
            return Err(error());
        }
        self.last = seq;
        if !self.created {
            if kind != "response.created" {
                return Err(error());
            }
            self.created = true;
        }
        if matches!(
            kind.as_str(),
            "error" | "response.failed" | "response.incomplete" | "response.cancelled"
        ) {
            return Err(AgentError::code(502, "RUNTIME_PROVIDER_REJECTED"));
        }
        if self.allowed.is_empty()
            && !self.search
            && ["function_call", "_search_call", "mcp_call"]
                .iter()
                .any(|s| kind.contains(s))
        {
            return Err(AgentError::code(502, "RUNTIME_TOOLS_NOT_ENABLED_FOR_PHASE"));
        }
        if kind == "response.function_call_arguments.delta" {
            let id = text(&event, "item_id");
            let delta = event["delta"].as_str().ok_or_else(error)?;
            if id.is_empty() || id.len() > 255 {
                return Err(error());
            }
            if !self.arguments.contains_key(id) && self.arguments.len() >= 256 {
                return Err(AgentError::code(502, "RUNTIME_PROVIDER_TOOL_LIMIT"));
            }
            let count = self.arguments.get(id).copied().unwrap_or(0) + delta.len();
            if count > 1024 * 1024 || self.argument_bytes + delta.len() > 4 * 1024 * 1024 {
                return Err(AgentError::code(
                    502,
                    "RUNTIME_PROVIDER_TOOL_ARGUMENTS_LIMIT",
                ));
            }
            self.argument_bytes += delta.len();
            self.arguments.insert(id.into(), count);
        }
        if let Some(item) = event.get_mut("item") {
            self.validate_item(item)?;
        }
        if kind != "response.completed" {
            return Ok(Some(frame(&kind, &event)));
        }
        if event["response"]["status"] != "completed" {
            return Err(error());
        }
        if let Some(output) = event["response"]["output"].as_array_mut() {
            for item in output {
                self.validate_item(item)?;
            }
        }
        let usage = &event["response"]["usage"];
        let input = usage["input_tokens"]
            .as_i64()
            .filter(|v| *v >= 0)
            .ok_or_else(|| AgentError::code(502, "RUNTIME_PROVIDER_USAGE_INVALID"))?;
        let output = usage["output_tokens"]
            .as_i64()
            .filter(|v| *v >= 0)
            .ok_or_else(|| AgentError::code(502, "RUNTIME_PROVIDER_USAGE_INVALID"))?;
        self.terminal = Some(Terminal {
            raw_usage: usage.clone(),
            frame: frame(&kind, &event),
            input,
            output,
            provider_request_id: event["response"]["id"].as_str().map(str::to_owned),
        });
        Ok(None)
    }
    pub fn finish(self) -> Result<Terminal, AgentError> {
        if !self.created {
            return Err(AgentError::code(502, "RUNTIME_PROVIDER_STREAM_INCOMPLETE"));
        }
        self.terminal
            .ok_or_else(|| AgentError::code(502, "RUNTIME_PROVIDER_STREAM_INCOMPLETE"))
    }
}
pub struct ChatProjector {
    model: String,
    input: i64,
    response: String,
    reasoning_id: String,
    message_id: String,
    seq: i64,
    text: String,
    reasoning: String,
    reasoning_open: bool,
    reasoning_closed: bool,
    message_open: bool,
    message_closed: bool,
    tools: BTreeMap<i64, Value>,
    added: BTreeSet<i64>,
    output: Vec<Value>,
    usage: Option<(i64, i64)>,
    raw_usage: Option<Value>,
    aliases: BTreeMap<String, (String, String)>,
    allowed: BTreeSet<(Option<String>, String)>,
    argument_bytes: usize,
}
impl ChatProjector {
    pub fn new(
        model: &str,
        input: i64,
        aliases: BTreeMap<String, (String, String)>,
        allowed: BTreeSet<(Option<String>, String)>,
    ) -> Self {
        Self {
            model: model.into(),
            input,
            response: format!("resp_{}", uuid::Uuid::new_v4().simple()),
            reasoning_id: format!("rs_{}", uuid::Uuid::new_v4().simple()),
            message_id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
            seq: 0,
            text: String::new(),
            reasoning: String::new(),
            reasoning_open: false,
            reasoning_closed: false,
            message_open: false,
            message_closed: false,
            tools: BTreeMap::new(),
            added: BTreeSet::new(),
            output: Vec::new(),
            usage: None,
            raw_usage: None,
            aliases,
            allowed,
            argument_bytes: 0,
        }
    }
    fn event(&mut self, kind: &str, mut payload: Value) -> Bytes {
        payload["type"] = kind.into();
        payload["sequence_number"] = self.seq.into();
        self.seq += 1;
        frame(kind, &payload)
    }
    pub fn created(&mut self) -> Bytes {
        self.event("response.created",json!({"response":{"id":self.response,"object":"response","created_at":chrono::Utc::now().timestamp(),"status":"in_progress","model":self.model,"output":[]}}))
    }
    fn reasoning_delta(&mut self, delta: &str) -> Result<Vec<Bytes>, AgentError> {
        let mut events = Vec::new();
        if !self.reasoning_open {
            self.reasoning_open = true;
            events.push(self.event("response.output_item.added",json!({"output_index":self.output.len(),"item":{"id":self.reasoning_id,"type":"reasoning","status":"in_progress","summary":[]}})));
            events.push(self.event("response.reasoning_summary_part.added",json!({"item_id":self.reasoning_id,"output_index":self.output.len(),"summary_index":0,"part":{"type":"summary_text","text":""}})));
        }
        if self.reasoning.len() + delta.len() > 8 * 1024 * 1024 {
            return Err(AgentError::code(502, "RUNTIME_PROVIDER_TEXT_LIMIT"));
        }
        self.reasoning.push_str(delta);
        events.push(self.event("response.reasoning_summary_text.delta",json!({"item_id":self.reasoning_id,"output_index":self.output.len(),"summary_index":0,"delta":delta})));
        Ok(events)
    }
    fn close_reasoning(&mut self) -> Vec<Bytes> {
        if !self.reasoning_open || self.reasoning_closed {
            return vec![];
        }
        let index = self.output.len();
        let item = json!({"id":self.reasoning_id,"type":"reasoning","status":"completed","summary":[{"type":"summary_text","text":self.reasoning}]});
        self.output.push(item.clone());
        self.reasoning_closed = true;
        vec![self.event("response.reasoning_summary_text.done",json!({"item_id":self.reasoning_id,"output_index":index,"summary_index":0,"text":self.reasoning})),self.event("response.reasoning_summary_part.done",json!({"item_id":self.reasoning_id,"output_index":index,"summary_index":0,"part":{"type":"summary_text","text":self.reasoning}})),self.event("response.output_item.done",json!({"output_index":index,"item":item}))]
    }
    fn text_delta(&mut self, delta: &str) -> Result<Vec<Bytes>, AgentError> {
        let mut events = self.close_reasoning();
        if !self.message_open {
            self.message_open = true;
            events.push(self.event("response.output_item.added",json!({"output_index":self.output.len(),"item":{"id":self.message_id,"type":"message","status":"in_progress","role":"assistant","content":[]}})));
            events.push(self.event("response.content_part.added",json!({"item_id":self.message_id,"output_index":self.output.len(),"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}})));
        }
        if self.text.len() + delta.len() > 8 * 1024 * 1024 {
            return Err(AgentError::code(502, "RUNTIME_PROVIDER_TEXT_LIMIT"));
        }
        self.text.push_str(delta);
        events.push(self.event("response.output_text.delta",json!({"item_id":self.message_id,"output_index":self.output.len(),"content_index":0,"delta":delta,"logprobs":[]})));
        Ok(events)
    }
    fn close_message(&mut self) -> Vec<Bytes> {
        if !self.message_open || self.message_closed {
            return vec![];
        }
        let index = self.output.len();
        let part = json!({"type":"output_text","text":self.text,"annotations":[]});
        let item = json!({"id":self.message_id,"type":"message","status":"completed","role":"assistant","content":[part]});
        self.output.push(item.clone());
        self.message_closed = true;
        vec![self.event("response.output_text.done",json!({"item_id":self.message_id,"output_index":index,"content_index":0,"text":self.text,"logprobs":[]})),self.event("response.content_part.done",json!({"item_id":self.message_id,"output_index":index,"content_index":0,"part":part})),self.event("response.output_item.done",json!({"output_index":index,"item":item}))]
    }
    fn open_tool(&mut self, index: i64) -> Result<Vec<Bytes>, AgentError> {
        if self.added.contains(&index) {
            return Ok(vec![]);
        }
        let call = self.tools.get_mut(&index).ok_or_else(error)?;
        let wire = text(call, "_wire_name").to_owned();
        if text(call, "call_id").is_empty() || wire.is_empty() {
            return Err(error());
        }
        let identity = self
            .aliases
            .get(&wire)
            .cloned()
            .map(|(ns, n)| (Some(ns), n))
            .unwrap_or((None, wire));
        if !self.allowed.contains(&identity) {
            return Err(AgentError::code(
                502,
                "RUNTIME_PROVIDER_TOOL_SCOPE_MISMATCH",
            ));
        }
        call["name"] = identity.1.into();
        if let Some(ns) = identity.0 {
            call["namespace"] = ns.into();
        }
        let mut item = call.clone();
        item.as_object_mut()
            .expect("call")
            .retain(|k, _| !k.starts_with('_'));
        item["arguments"] = "".into();
        self.added.insert(index);
        Ok(vec![self.event(
            "response.output_item.added",
            json!({"output_index":self.output.len()as i64+index,"item":item}),
        )])
    }
    fn tool_delta(&mut self, raw: &Value) -> Result<Vec<Bytes>, AgentError> {
        let calls = raw.as_array().ok_or_else(error)?;
        let mut events = self.close_reasoning();
        for raw in calls {
            let index = raw.get("index").and_then(Value::as_i64).unwrap_or(0);
            if index < 0 || !raw["function"].is_object() {
                return Err(error());
            }
            if !self.tools.contains_key(&index) && self.tools.len() >= 256 {
                return Err(AgentError::code(502, "RUNTIME_PROVIDER_TOOL_LIMIT"));
            }
            let id = raw.get("id").filter(|v| !v.is_null());
            if let Some(id) = id {
                let s = id
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 255)
                    .ok_or_else(error)?;
                if self
                    .tools
                    .iter()
                    .any(|(i, c)| *i != index && c["call_id"] == s)
                {
                    return Err(error());
                }
            }
            let call=self.tools.entry(index).or_insert_with(||json!({"id":"","type":"function_call","status":"in_progress","call_id":"","name":"","arguments":"","_wire_name":"","_pending_arguments":""}));
            if let Some(id) = id {
                if !text(call, "call_id").is_empty() && call["call_id"] != *id {
                    return Err(error());
                }
                call["id"] = id.clone();
                call["call_id"] = id.clone();
            }
            if let Some(name) = raw["function"].get("name").filter(|v| !v.is_null()) {
                let name = name.as_str().ok_or_else(error)?;
                let previous = text(call, "_wire_name");
                let wire = if name == previous {
                    previous.into()
                } else {
                    format!("{previous}{name}")
                };
                if wire.is_empty()
                    || wire.len() > 64
                    || !wire
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                    || (self.added.contains(&index) && wire != previous)
                {
                    return Err(error());
                }
                let allowed_wire = self.allowed.iter().any(|(ns, n)| {
                    if let Some(ns) = ns {
                        self.aliases.iter().any(|(alias, identity)| {
                            identity == &(ns.clone(), n.clone()) && alias.starts_with(&wire)
                        })
                    } else {
                        n.starts_with(&wire)
                    }
                });
                if !allowed_wire {
                    return Err(AgentError::code(
                        502,
                        "RUNTIME_PROVIDER_TOOL_SCOPE_MISMATCH",
                    ));
                }
                call["_wire_name"] = wire.into();
            }
            if let Some(args) = raw["function"].get("arguments").filter(|v| !v.is_null()) {
                let args = args.as_str().ok_or_else(error)?;
                let old = text(call, "arguments");
                if old.len() + args.len() > 1024 * 1024
                    || self.argument_bytes + args.len() > 4 * 1024 * 1024
                {
                    return Err(AgentError::code(
                        502,
                        "RUNTIME_PROVIDER_TOOL_ARGUMENTS_LIMIT",
                    ));
                }
                self.argument_bytes += args.len();
                call["arguments"] = format!("{old}{args}").into();
                call["_pending_arguments"] =
                    format!("{}{args}", text(call, "_pending_arguments")).into();
            }
            let ready = !text(call, "call_id").is_empty()
                && !text(call, "_wire_name").is_empty()
                && !text(call, "_pending_arguments").is_empty();
            if ready {
                let id = call["call_id"].clone();
                let delta = call["_pending_arguments"].clone();
                call["_pending_arguments"] = "".into();
                events.extend(self.open_tool(index)?);
                events.push(self.event("response.function_call_arguments.delta",json!({"item_id":id,"output_index":self.output.len()as i64+index,"delta":delta})));
            }
        }
        Ok(events)
    }
    pub fn consume(&mut self, chunk: &Value) -> Result<Vec<Bytes>, AgentError> {
        if !chunk.is_object() || chunk.get("error").is_some() {
            return Err(AgentError::code(502, "RUNTIME_PROVIDER_REJECTED"));
        }
        let usage = &chunk["usage"];
        let input = usage["prompt_tokens"]
            .as_i64()
            .or_else(|| usage["input_tokens"].as_i64());
        let output = usage["completion_tokens"]
            .as_i64()
            .or_else(|| usage["output_tokens"].as_i64());
        if let (Some(i), Some(o)) = (input, output) {
            if i >= 0 && o >= 0 {
                self.usage = Some((i, o));
                self.raw_usage = Some(usage.clone());
            }
        }
        let mut frames = Vec::new();
        for choice in chunk["choices"].as_array().into_iter().flatten() {
            let delta = &choice["delta"];
            if let Some(s) = delta["reasoning_content"]
                .as_str()
                .or_else(|| delta["reasoning"].as_str())
                .filter(|s| !s.is_empty())
            {
                frames.extend(self.reasoning_delta(s)?);
            }
            if let Some(s) = delta["content"].as_str().filter(|s| !s.is_empty()) {
                frames.extend(self.text_delta(s)?);
            }
            if let Some(calls) = delta.get("tool_calls").filter(|v| !v.is_null()) {
                frames.extend(self.tool_delta(calls)?);
            }
            if matches!(text(choice, "finish_reason"), "content_filter" | "error") {
                return Err(AgentError::code(502, "RUNTIME_PROVIDER_REJECTED"));
            }
        }
        Ok(frames)
    }
    pub fn raw_usage(&self) -> Value {
        self.raw_usage.clone().unwrap_or(Value::Null)
    }
    pub fn complete(&mut self) -> Result<(Vec<Bytes>, i64, i64), AgentError> {
        let mut events = self.close_reasoning();
        let base = self.output.len();
        let indices: Vec<_> = self.tools.keys().copied().collect();
        for index in indices {
            events.extend(self.open_tool(index)?);
            let mut call = self.tools[&index].clone();
            call.as_object_mut()
                .expect("call")
                .retain(|k, _| !k.starts_with('_'));
            let position = base as i64 + index;
            events.push(self.event("response.function_call_arguments.done",json!({"item_id":call["call_id"],"output_index":position,"name":call["name"],"arguments":call["arguments"]})));
            call["status"] = "completed".into();
            events.push(self.event(
                "response.output_item.done",
                json!({"output_index":position,"item":call}),
            ));
            self.output.push(call);
        }
        events.extend(self.close_message());
        if self.text.is_empty() && self.tools.is_empty() {
            return Err(AgentError::code(502, "RUNTIME_PROVIDER_EMPTY_RESPONSE"));
        }
        let (input, output) = self
            .usage
            .unwrap_or((self.input, estimate_tokens(&json!(self.text))));
        let mut usage = self.raw_usage.clone().unwrap_or_else(|| json!({}));
        usage["input_tokens"] = input.into();
        usage["output_tokens"] = output.into();
        usage["total_tokens"] = (input + output).into();
        if let Some(details) = usage.get("prompt_tokens_details").cloned() {
            usage["input_tokens_details"] = details;
        }
        usage["cache_usage_state"] =
            if usage["input_tokens_details"].get("cached_tokens").is_some() {
                "reported"
            } else {
                "not_reported"
            }
            .into();
        events.push(self.event("response.completed",json!({"response":{"id":self.response,"object":"response","created_at":chrono::Utc::now().timestamp(),"status":"completed","error":null,"incomplete_details":null,"model":self.model,"output":self.output,"usage":usage}})));
        events.push(Bytes::from_static(b"data: [DONE]\n\n"));
        Ok((events, input, output))
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    #[test]
    fn chat_cache_details_and_vendor_fields_survive_projection() {
        let mut p = ChatProjector::new("m", 1, BTreeMap::new(), BTreeSet::new());
        p.consume(&json!({"choices":[{"delta":{"content":"ok"}}],"usage":{"prompt_tokens":100,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":80,"vendor_field":"kept"},"vendor_usage":7}})).unwrap();
        let (frames, _, _) = p.complete().unwrap();
        let completed = frames
            .iter()
            .map(|v| String::from_utf8_lossy(v))
            .find(|v| v.contains("response.completed"))
            .unwrap();
        assert!(completed.contains("\"cached_tokens\":80"));
        assert!(completed.contains("vendor_field"));
        assert!(completed.contains("vendor_usage"));
    }
}
