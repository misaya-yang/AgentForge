//! Text-only choice-zero QA completion. Service success requires semantic `stop`;
//! `[DONE]` alone only closes transport framing. No provider I/O or automatic retry.
use serde_json::Value;

const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;
#[derive(Debug, Default)]
pub struct ProviderEvent {
    pub content: Option<String>,
    pub tokens: Option<u64>,
    pub closed: bool,
}
#[derive(Default)]
pub struct QaProviderStream {
    data: String,
    stopped: bool,
    closed: bool,
}
impl QaProviderStream {
    pub fn line(&mut self, line: &str) -> Result<Option<ProviderEvent>, &'static str> {
        if self.closed { return Err("qa_stream_after_terminal"); }
        if line.len() > MAX_EVENT_BYTES { return Err("qa_stream_event_too_large"); }
        if line.is_empty() { return self.dispatch(); }
        if line.starts_with(':') { return Ok(None); }
        if let Some(data) = line.strip_prefix("data:") {
            let data = data.strip_prefix(' ').unwrap_or(data);
            if self.data.len() + data.len() + 1 > MAX_EVENT_BYTES { return Err("qa_stream_event_too_large"); }
            self.data.push_str(data); self.data.push('\n');
            return Ok(None);
        }
        if line.starts_with("event:") || line.starts_with("id:") || line.starts_with("retry:") {
            return Ok(None);
        }
        // Preserve the existing line-delimited JSON compatibility, without
        // accidentally merging a bare frame into an unfinished SSE event.
        if !self.data.is_empty() { return Err("qa_stream_invalid"); }
        self.parse(line.trim()).map(Some)
    }
    pub fn finish(&mut self) -> Result<Option<ProviderEvent>, &'static str> {
        let event = self.dispatch()?;
        if !self.stopped { return Err("qa_stream_incomplete"); }
        Ok(event)
    }
    fn dispatch(&mut self) -> Result<Option<ProviderEvent>, &'static str> {
        let data = std::mem::take(&mut self.data);
        if data.trim().is_empty() { return Ok(None); }
        self.parse(data.trim()).map(Some)
    }
    fn parse(&mut self, raw: &str) -> Result<ProviderEvent, &'static str> {
        if raw == "[DONE]" {
            if !self.stopped { return Err("qa_stream_incomplete"); }
            self.closed = true;
            return Ok(ProviderEvent { closed: true, ..Default::default() });
        }
        let value: Value = serde_json::from_str(raw).map_err(|_| "qa_stream_invalid")?;
        let object = value.as_object().ok_or("qa_stream_invalid")?;
        if object.get("error").is_some_and(|v| !v.is_null()) { return Err("qa_stream_provider_error"); }
        let tokens = match object.get("usage").filter(|v| !v.is_null()) {
            None => None,
            Some(usage) => {
                let usage = usage.as_object().ok_or("qa_stream_invalid")?;
                usage.get("total_tokens").filter(|v| !v.is_null()).map(|v| v.as_u64().ok_or("qa_stream_invalid")).transpose()?
            }
        };
        let choices = object.get("choices").and_then(Value::as_array).ok_or("qa_stream_invalid")?;
        if choices.is_empty() { return Ok(ProviderEvent { tokens, ..Default::default() }); }
        if choices.len() != 1 { return Err("qa_stream_invalid"); }
        let choice = choices[0].as_object().ok_or("qa_stream_invalid")?;
        if choice.get("index").is_some_and(|v| v.as_u64() != Some(0)) { return Err("qa_stream_invalid"); }
        let delta = choice.get("delta").and_then(Value::as_object).ok_or("qa_stream_invalid")?;
        if delta.get("tool_calls").is_some_and(|v| !v.is_null()) || delta.get("function_call").is_some_and(|v| !v.is_null()) {
            return Err("qa_stream_unsupported_finish");
        }
        let content = delta.get("content").filter(|v| !v.is_null()).map(|v| v.as_str().map(str::to_owned).ok_or("qa_stream_invalid")).transpose()?;
        if self.stopped && content.as_ref().is_some_and(|s| !s.is_empty()) { return Err("qa_stream_after_terminal"); }
        if let Some(reason) = choice.get("finish_reason").filter(|v| !v.is_null()) {
            if reason.as_str() != Some("stop") { return Err("qa_stream_unsupported_finish"); }
            self.stopped = true;
        }
        Ok(ProviderEvent { content, tokens, closed: false })
    }
}
