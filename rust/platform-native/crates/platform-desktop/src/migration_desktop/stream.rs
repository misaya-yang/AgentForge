//! Single-admission SSE for canonical preview/QA streams and cursor-based document progress.
use super::api::{ApiError, ApiRequest, DesktopApi};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
#[derive(Clone, Debug)]
pub struct StreamEvent {
    pub id: Option<String>,
    pub event: String,
    pub data: Value,
}
#[derive(Default)]
struct Decoder {
    buffer: Vec<u8>,
    data: Vec<String>,
    event: String,
    last_id: Option<String>,
    frame_bytes: usize,
}
impl Decoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<StreamEvent>, ApiError> {
        if self.buffer.len() + bytes.len() > 1024 * 1024 {
            return Err(ApiError::Response);
        }
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
            let bytes: Vec<_> = self.buffer.drain(..=end).collect();
            let line = std::str::from_utf8(&bytes[..bytes.len() - 1])
                .map_err(|_| ApiError::Response)?
                .trim_end_matches('\r');
            self.line(line, &mut events)?;
        }
        Ok(events)
    }
    fn line(&mut self, line: &str, out: &mut Vec<StreamEvent>) -> Result<(), ApiError> {
        self.frame_bytes += line.len();
        if self.frame_bytes > 1024 * 1024 {
            return Err(ApiError::Response);
        }
        if line.is_empty() {
            if !self.data.is_empty() {
                let text = self.data.join("\n");
                self.data.clear();
                if text != "[DONE]" {
                    let data = serde_json::from_str(&text).map_err(|_| ApiError::Response)?;
                    out.push(StreamEvent {
                        id: self.last_id.clone(),
                        event: if self.event.is_empty() {
                            "message".into()
                        } else {
                            std::mem::take(&mut self.event)
                        },
                        data,
                    });
                }
            }
            self.event.clear();
            self.frame_bytes = 0;
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, raw) = line.split_once(':').unwrap_or((line, ""));
        let value = raw.strip_prefix(' ').unwrap_or(raw);
        match field {
            "data" => self.data.push(value.into()),
            "event" => self.event = value.into(),
            "id" if !value.contains('\0') => self.last_id = Some(value.into()),
            _ => {}
        }
        Ok(())
    }
    fn finish(mut self) -> Result<Vec<StreamEvent>, ApiError> {
        let mut out = Vec::new();
        let remaining = std::mem::take(&mut self.buffer);
        if !remaining.is_empty() {
            self.line(
                std::str::from_utf8(&remaining)
                    .map_err(|_| ApiError::Response)?
                    .trim_end_matches('\r'),
                &mut out,
            )?;
        }
        self.line("", &mut out)?;
        Ok(out)
    }
}
impl DesktopApi {
    /// POST streams are sent once. The caller may explicitly reconnect GET streams
    /// with Last-Event-ID; closing observation never claims server cancellation.
    pub async fn stream_once(
        &self,
        request: ApiRequest,
        sender: mpsc::Sender<Result<StreamEvent, ApiError>>,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<(), ApiError> {
        let mutation = request.is_mutation();
        let url = self.endpoint(&request.segments)?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| ApiError::Transport)?;
        let mut pending = http
            .request(request.method, url)
            .query(&request.query)
            .bearer_auth(self.token.as_ref())
            .header("Accept", "text/event-stream");
        for (name, value) in request.headers {
            if ![" if-match", "idempotency-key", "last-event-id"]
                .contains(&name.to_ascii_lowercase().as_str())
                || value.contains(['\r', '\n'])
            {
                return Err(ApiError::Request);
            }
            pending = pending.header(name, value);
        }
        if let Some(body) = request.body {
            pending = pending.json(&body);
        }
        if mutation {
            pending = pending.timeout(Duration::from_secs(300));
        }
        let mut response = tokio::select! {_ = cancel.changed()=>return Ok(()),response=pending.send()=>response.map_err(|_|ApiError::Transport)?};
        if !response.status().is_success() {
            return Err(ApiError::Status(response.status().as_u16()));
        }
        if response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(str::trim)
            != Some("text/event-stream")
        {
            return Err(ApiError::Response);
        }
        let mut decoder = Decoder::default();
        loop {
            let chunk = tokio::select! {_ = cancel.changed()=>return Ok(()),chunk=response.chunk()=>chunk.map_err(|_|ApiError::Transport)?};
            let (eof, frames) = match chunk {
                Some(chunk) => (false, decoder.push(&chunk)?),
                None => (true, std::mem::take(&mut decoder).finish()?),
            };
            for frame in frames {
                tokio::select! {_ = cancel.changed()=>return Ok(()),result=sender.send(Ok(frame))=>if result.is_err(){return Ok(());}}
            }
            if eof {
                return Ok(());
            }
        }
    }
}
