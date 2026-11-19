//! Python SDK-compatible event/data projection without conflating V2 outer envelopes.
use crate::Error;
use serde_json::{Value, json};
#[derive(Debug, Clone, PartialEq)]
pub struct SdkEvent {
    pub event_type: String,
    pub data: Value,
    pub timestamp: Option<f64>,
}
fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
impl SdkEvent {
    pub fn is_text(&self) -> bool {
        self.event_type == "text_delta"
    }
    pub fn is_done(&self) -> bool {
        matches!(self.event_type.as_str(), "done" | "run_finished")
    }
    pub fn is_error(&self) -> bool {
        matches!(self.event_type.as_str(), "error" | "run_error")
    }
    pub fn is_cancelled(&self) -> bool {
        self.event_type == "cancelled"
    }
    pub fn text(&self) -> String {
        let value = self.data.get("content").or_else(|| self.data.get("text"));
        match value {
            Some(Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
            None => String::new(),
        }
    }
    pub fn terminal(&self) -> bool {
        matches!(
            self.event_type.as_str(),
            "done" | "error" | "cancelled" | "run_finished" | "run_error"
        )
    }
    pub fn parse(event_field: Option<&str>, raw: &str) -> Result<Self, Error> {
        if raw.trim() == "[DONE]" {
            return Ok(Self {
                event_type: "done".into(),
                data: json!({}),
                timestamp: Some(now()),
            });
        }
        let mut payload: Value = serde_json::from_str(raw).unwrap_or_else(|_| json!({"raw":raw}));
        let object = payload.as_object_mut().ok_or(Error::Response)?;
        let event_type = event_field
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                object
                    .remove("event_type")
                    .and_then(|v| v.as_str().map(str::to_owned))
            })
            .or_else(|| {
                object
                    .remove("event")
                    .and_then(|v| v.as_str().map(str::to_owned))
            })
            .unwrap_or_else(|| "message".into());
        let timestamp = Some(match object.remove("timestamp") {
            Some(Value::String(s)) => chrono::DateTime::parse_from_rfc3339(&s)
                .map(|v| v.timestamp_micros() as f64 / 1_000_000.)
                .unwrap_or_else(|_| now()),
            Some(Value::Number(n)) => n.as_f64().ok_or(Error::Response)?,
            Some(Value::Bool(b)) => {
                if b {
                    1.
                } else {
                    0.
                }
            }
            None | Some(Value::Null) => now(),
            _ => return Err(Error::Response),
        });
        let data = match object.remove("data") {
            Some(Value::String(s)) => json!({"content":s}),
            Some(v @ Value::Object(_)) => v,
            Some(v) => json!({"value":v}),
            None => payload,
        };
        Ok(Self {
            event_type,
            data,
            timestamp,
        })
    }
}
/// Incremental SSE framing keeps the explicit event name used by the Python SDK.
#[derive(Default)]
pub struct SdkDecoder {
    pending: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
    size: usize,
}
impl SdkDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SdkEvent>, Error> {
        if self.pending.len().saturating_add(bytes.len()) > 1_048_576 {
            return Err(Error::Response);
        }
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = self.pending.drain(..=pos).collect();
            self.line(
                std::str::from_utf8(&line[..line.len() - 1])
                    .map_err(|_| Error::Response)?
                    .trim_end_matches('\r'),
                &mut out,
            )?;
        }
        Ok(out)
    }
    fn line(&mut self, line: &str, out: &mut Vec<SdkEvent>) -> Result<(), Error> {
        self.size = self.size.saturating_add(line.len());
        if self.size > 1_048_576 {
            return Err(Error::Response);
        }
        if line.is_empty() {
            if !self.data.is_empty() {
                out.push(SdkEvent::parse(
                    self.event.as_deref(),
                    &self.data.join("\n"),
                )?);
            }
            self.data.clear();
            self.event = None;
            self.size = 0;
        } else if let Some(s) = line.strip_prefix("data:") {
            self.data.push(s.trim_start_matches(' ').into());
        } else if let Some(s) = line.strip_prefix("event:") {
            self.event = Some(s.trim().into());
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<Vec<SdkEvent>, Error> {
        let pending = std::mem::take(&mut self.pending);
        let mut out = Vec::new();
        if !pending.is_empty() {
            self.line(
                std::str::from_utf8(&pending)
                    .map_err(|_| Error::Response)?
                    .trim_end_matches('\r'),
                &mut out,
            )?;
        }
        self.line("", &mut out)?;
        Ok(out)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_sdk_fixture() {
        let f: Value = serde_json::from_str(include_str!(
            "../../../../../sdk/fixtures/sse_inner_envelopes.json"
        ))
        .unwrap();
        for (name, case) in f.as_object().unwrap() {
            let Some(sse) = case.get("sse").and_then(Value::as_str) else {
                continue;
            };
            let mut decoder = SdkDecoder::default();
            let mut events = Vec::new();
            for byte in sse.as_bytes() {
                events.extend(decoder.push(&[*byte]).unwrap());
            }
            events.extend(decoder.finish().unwrap());
            assert_eq!(events.len(), 1, "{name}");
            if let Some(v) = case.get("value") {
                assert_eq!(events[0].data, json!({"value":v}), "{name}");
            }
        }
    }
    #[test]
    fn explicit_event_has_precedence() {
        let e = SdkEvent::parse(Some("done"), r#"{"event_type":"message","data":null}"#).unwrap();
        assert!(e.terminal());
        assert_eq!(e.data, json!({"value":null}));
    }
}

/// Shared bounded body lifecycle for single-admission SDK streams. This never
/// resubmits a POST or invents a resume cursor for an unsequenced token stream.
pub(crate) async fn receive(
    mut response: reqwest::Response,
    sender: tokio::sync::mpsc::Sender<Result<SdkEvent, Error>>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    project: fn(SdkEvent) -> Option<SdkEvent>,
    limits: crate::StreamLimits,
) -> Result<(), Error> {
    if !response.status().is_success() {
        return Err(Error::Status(response.status().as_u16()));
    }
    let content = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next());
    if !content.is_some_and(|v| v.trim().eq_ignore_ascii_case("text/event-stream")) {
        return Err(Error::Response);
    }
    let mut decoder = SdkDecoder::default();
    loop {
        if *cancel.borrow() || sender.is_closed() {
            return Ok(());
        }
        let chunk = tokio::select! {
            _=cancel.changed()=>return Ok(()),
            _=sender.closed()=>return Ok(()),
            result=tokio::time::timeout(limits.idle_timeout(),response.chunk())=>result.map_err(|_|Error::Transport)?.map_err(|_|Error::Transport)?,
        };
        let eof = chunk.is_none();
        let events = match chunk {
            Some(bytes) => decoder.push(&bytes)?,
            None => std::mem::take(&mut decoder).finish()?,
        };
        for event in events.into_iter().filter_map(project) {
            let terminal = event.terminal();
            tokio::select! {
                _=cancel.changed()=>return Ok(()),
                _=sender.closed()=>return Ok(()),
                result=sender.send(Ok(event))=>if result.is_err(){return Ok(())},
            }
            if terminal {
                return Ok(());
            }
        }
        if eof {
            return Ok(());
        }
    }
}
