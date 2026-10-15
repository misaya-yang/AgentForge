use super::*;

/// Raw structured events retain tool call IDs, function arguments and usage.
/// No tool is executed here. Failed/incomplete/error events are returned as errors.
#[derive(Clone, Serialize, Deserialize)]
pub struct ResponseEvent {
    pub event_type: String,
    pub data: Value,
}
impl fmt::Debug for ResponseEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponseEvent")
            .field("event_type", &self.event_type)
            .finish_non_exhaustive()
    }
}
pub struct ResponseStream {
    response: Response,
    token: SecretString,
    buffer: Vec<u8>,
    terminal: bool,
    request_id: Option<String>,
    bytes_read: usize,
}
impl ResponseStream {
    pub(crate) fn new(response: Response, token: SecretString) -> Self {
        let request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        Self {
            response,
            token,
            buffer: Vec::new(),
            terminal: false,
            request_id,
            bytes_read: 0,
        }
    }
    /// Only response.completed establishes success. EOF, [DONE], failure and incomplete do not.
    pub async fn next(&mut self) -> Result<Option<ResponseEvent>> {
        if self.terminal {
            return Ok(None);
        }
        loop {
            if let Some((end, delimiter_len)) = frame_boundary(&self.buffer) {
                let frame = self.buffer.drain(..end + delimiter_len).collect::<Vec<_>>();
                let data = match parse_frame(&frame[..end]) {
                    Ok(v) => v,
                    Err(e) => {
                        self.terminal = true;
                        return Err(e);
                    }
                };
                let Some(data) = data else {
                    continue;
                };
                let event_type = data
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        self.terminal = true;
                        PlanError::local("missing_event_type")
                    })?
                    .to_owned();
                let data = redact(data, &[self.token.expose()]);
                match event_type.as_str() {
                    "response.completed" => self.terminal = true,
                    "response.failed" | "response.incomplete" | "error" => {
                        self.terminal = true;
                        return Err(PlanError {
                            kind: match event_type.as_str() {
                                "response.failed" => "response_failed",
                                "response.incomplete" => "response_incomplete",
                                _ => "stream_error",
                            },
                            status: Some(200),
                            request_id: self.request_id.clone(),
                            diagnostic: Some(data),
                        });
                    }
                    _ => {}
                }
                return Ok(Some(ResponseEvent { event_type, data }));
            }
            let chunk = match self.response.chunk().await {
                Ok(Some(c)) => c,
                Ok(None) => {
                    self.terminal = true;
                    return Err(PlanError::local("stream_interrupted"));
                }
                Err(_) => {
                    self.terminal = true;
                    return Err(PlanError::local("stream_transport"));
                }
            };
            self.bytes_read = self.bytes_read.saturating_add(chunk.len());
            if self.buffer.len().saturating_add(chunk.len()) > 2 * MAX_JSON
                || self.bytes_read > 64 * MAX_JSON
            {
                self.terminal = true;
                return Err(PlanError::local("stream_limit_exceeded"));
            }
            self.buffer.extend_from_slice(&chunk);
        }
    }
}
fn frame_boundary(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes.windows(2).position(|w| w == b"\n\n").map(|p| (p, 2));
    let crlf = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| (p, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 < b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}
fn parse_frame(bytes: &[u8]) -> Result<Option<Value>> {
    if bytes.len() > MAX_JSON {
        return Err(PlanError::local("event_too_large"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| PlanError::local("invalid_event_utf8"))?;
    let data = text
        .lines()
        .filter_map(|line| {
            line.strip_prefix("data:")
                .map(|v| v.strip_prefix(' ').unwrap_or(v))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        return Ok(None);
    }
    if data == "[DONE]" {
        return Err(PlanError::local("stream_without_completion"));
    }
    serde_json::from_str(&data)
        .map(Some)
        .map_err(|_| PlanError::local("invalid_event_json"))
}
