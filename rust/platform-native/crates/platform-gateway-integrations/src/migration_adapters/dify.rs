//! Opt-in Dify chat protocol. The legacy `dify` generic mapping remains intact.
use super::*;
use reqwest::header::HeaderMap;
use sha2::{Digest, Sha256};

const MAX_EVENT_BYTES: usize = 1024 * 1024;

pub struct DifyChatAdapter {
    pub http: HttpTransport,
}

fn payload(request: &UnifiedRequest, streaming: bool) -> Result<Value, AdapterError> {
    if request.user_id.trim().is_empty() || request.tenant_id.trim().is_empty() {
        return Err(validation("dify_authenticated_identity_required"));
    }
    if request.inputs.is_empty()
        || request
            .inputs
            .iter()
            .any(|v| v.content_type != ContentType::Text)
    {
        return Err(validation("dify_chat_requires_text_inputs"));
    }
    let texts = request
        .inputs
        .iter()
        .map(|item| match &item.data {
            Some(ContentData::Text(text)) => Ok(text.as_str()),
            _ => Err(validation("dify_chat_requires_text_inputs")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let query = texts.join("\n");
    if query.trim().is_empty() {
        return Err(validation("dify_chat_requires_text_inputs"));
    }
    // Dify authorizes conversations by app and end-user. Never accept a caller's
    // `user` override or share the same Dify user across platform tenants.
    let identity = serde_json::to_vec(&(&request.tenant_id, &request.user_id))
        .map_err(|_| validation("dify_identity_invalid"))?;
    let user = format!("platform-{:x}", Sha256::digest(identity));
    let mut body = json!({"query":query,"inputs":{},"user":user,
        "response_mode":if streaming {"streaming"} else {"blocking"}});
    for (key, value) in request.parameters.iter().flatten() {
        let valid = match key.as_str() {
            "inputs" => value.is_object(),
            "conversation_id" | "workflow_id" => value.is_string(),
            "auto_generate_name" => value.is_boolean(),
            _ => return Err(validation("dify_chat_parameter_unsupported")),
        };
        if !valid {
            return Err(validation("dify_chat_parameter_invalid"));
        }
        body[key] = value.clone();
    }
    Ok(body)
}

fn metadata(event: &Value) -> Map<String, Value> {
    let mut result = Map::new();
    for key in ["conversation_id", "message_id", "task_id"] {
        if let Some(value) = event.get(key).filter(|v| v.is_string()) {
            result.insert(format!("dify_{key}"), value.clone());
        }
    }
    if let Some(value) = event.get("metadata").filter(|v| v.is_object()) {
        result.insert("dify_metadata".into(), value.clone());
        if let Some(usage) = value.get("usage").filter(|v| v.is_object()) {
            result.insert("usage".into(), usage.clone());
        }
    }
    result
}

impl ProtocolAdapter for DifyChatAdapter {
    fn invoke<'a>(&'a self, request: &'a UnifiedRequest) -> AdapterFuture<'a, UnifiedResponse> {
        Box::pin(async move {
            let response = self
                .http
                .post_json("chat-messages", &payload(request, false)?, HeaderMap::new())
                .await?
                .json()
                .await?;
            if response.get("event").and_then(Value::as_str) == Some("error") {
                return Err(validation("dify_provider_error"));
            }
            let answer = response
                .get("answer")
                .and_then(Value::as_str)
                .ok_or_else(|| validation("dify_response_answer_missing"))?;
            let mut result = UnifiedResponse::new(
                &request.request_id,
                "success",
                vec![content(ContentType::Text, json!(answer))],
            );
            // Platform session IDs and upstream conversation IDs are distinct.
            result.session_id = request.session_id.clone();
            result.usage = response["metadata"]["usage"].as_object().cloned();
            result.metadata = Some(metadata(&response));
            Ok(result)
        })
    }
    fn stream(
        &self,
        request: UnifiedRequest,
    ) -> AdapterFuture<'_, Box<dyn DispatchProducer<StreamChunk>>> {
        Box::pin(async move {
            let response = self
                .http
                .post_json("chat-messages", &payload(&request, true)?, HeaderMap::new())
                .await?;
            // The event budget excludes the optional SSE "data: " prefix.
            let lines = Lines::with_max_line_bytes(response, MAX_EVENT_BYTES + 6)?;
            Ok(Box::new(DifyStream {
                lines,
                request_id: request.request_id,
                index: 0,
                finished: false,
                data: String::new(),
                metadata: Map::new(),
                agent_answer: String::new(),
                saw_agent: false,
                workflow: false,
                message_end: false,
            }) as Box<dyn DispatchProducer<StreamChunk>>)
        })
    }
    fn health_check(&self) -> AdapterFuture<'_, bool> {
        Box::pin(async move {
            let url = self.http.url("info")?;
            let response = self.http.send(self.http.client.get(url)).await;
            // Unlike generic health, authentication failures are unhealthy.
            Ok(match response {
                Ok(response) => response.check_status().is_ok() && response.json().await.is_ok(),
                Err(_) => false,
            })
        })
    }
}

struct DifyStream {
    lines: Lines,
    request_id: String,
    index: i64,
    finished: bool,
    data: String,
    metadata: Map<String, Value>,
    agent_answer: String,
    saw_agent: bool,
    workflow: bool,
    message_end: bool,
}
impl DifyStream {
    fn emit(&mut self, text: &str, final_chunk: bool) -> StreamChunk {
        let mut result = chunk(
            &self.request_id,
            self.index,
            json!(text),
            if final_chunk {
                StreamEventType::Final
            } else {
                StreamEventType::TextDelta
            },
            final_chunk,
        );
        self.index += 1;
        result.metadata = Some(self.metadata.clone());
        result
    }
    fn finish(&mut self) -> StreamChunk {
        self.finished = true;
        self.lines.close();
        self.emit("", true)
    }
    fn observe(&mut self, event: Value) -> Result<Option<StreamChunk>, AdapterError> {
        let kind = event
            .get("event")
            .and_then(Value::as_str)
            .ok_or_else(|| validation("dify_stream_event_invalid"))?;
        self.metadata.extend(metadata(&event));
        match kind {
            "error" => Err(validation("dify_provider_error")),
            "message_replace" => Err(validation("dify_message_replacement_unsupported")),
            "human_input_required" | "workflow_paused" => {
                Err(validation("dify_human_input_unsupported"))
            }
            "workflow_started" => {
                self.workflow = true;
                Ok(None)
            }
            "workflow_finished" => {
                if event["data"]["status"].as_str() != Some("succeeded") || !self.message_end {
                    return Err(validation("dify_workflow_incomplete"));
                }
                Ok(Some(self.finish()))
            }
            "message_end" => {
                self.message_end = true;
                if self.workflow {
                    Ok(None)
                } else {
                    Ok(Some(self.finish()))
                }
            }
            "message" | "agent_message" => {
                let answer = event
                    .get("answer")
                    .and_then(Value::as_str)
                    .ok_or_else(|| validation("dify_stream_answer_missing"))?;
                // Agent apps repeat their complete answer in a closing message.
                // Emit only a new suffix, never duplicate the streamed answer.
                let text = if kind == "message" && self.saw_agent {
                    answer
                        .strip_prefix(&self.agent_answer)
                        .ok_or_else(|| validation("dify_agent_answer_replaced"))?
                        .to_owned()
                } else {
                    if kind == "agent_message" {
                        self.saw_agent = true;
                        if self.agent_answer.len().saturating_add(answer.len()) > 4 * 1024 * 1024 {
                            return Err(validation("dify_agent_answer_too_large"));
                        }
                        self.agent_answer.push_str(answer);
                    }
                    answer.to_owned()
                };
                Ok(if text.is_empty() {
                    None
                } else {
                    Some(self.emit(&text, false))
                })
            }
            // Progress, tool reasoning, citations and audio are not text deltas.
            _ => Ok(None),
        }
    }
    async fn read(&mut self) -> Result<Option<StreamChunk>, AdapterError> {
        if self.finished {
            return Ok(None);
        }
        loop {
            let line = self.lines.next().await?;
            match line {
                Some(line) if line.is_empty() => {
                    if self.data.is_empty() {
                        continue;
                    }
                    let data = std::mem::take(&mut self.data);
                    let event = serde_json::from_str(&data)
                        .map_err(|_| validation("dify_stream_json_invalid"))?;
                    if let Some(chunk) = self.observe(event)? {
                        return Ok(Some(chunk));
                    }
                }
                Some(line) => {
                    if let Some(data) = line.strip_prefix("data:") {
                        let data = data.strip_prefix(' ').unwrap_or(data);
                        let separator_bytes = usize::from(!self.data.is_empty());
                        if self.data.len().saturating_add(separator_bytes)
                            .saturating_add(data.len()) > MAX_EVENT_BYTES
                        {
                            return Err(validation("dify_stream_event_too_large"));
                        }
                        if !self.data.is_empty() {
                            self.data.push('\n');
                        }
                        self.data.push_str(data);
                    }
                }
                None => return Err(validation("dify_stream_incomplete")),
            }
        }
    }
}
impl DispatchProducer<StreamChunk> for DifyStream {
    fn next(&mut self) -> DispatchFuture<'_, Option<StreamChunk>> {
        Box::pin(async move {
            match self.read().await {
                Ok(value) => Ok(value),
                Err(error) => {
                    self.lines.close();
                    self.finished = true;
                    self.data.clear();
                    Err(dispatch(error))
                }
            }
        })
    }
    fn close(mut self: Box<Self>) -> DispatchFuture<'static, ()> {
        Box::pin(async move {
            // Cancels the HTTP read and releases its admission permit. Dify's
            // remote stop endpoint is deliberately not implied by local close.
            self.lines.close();
            self.finished = true;
            self.data.clear();
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    fn fixture(body: reqwest::Body) -> DifyStream {
        let response = axum::http::Response::builder().status(200).body(body).unwrap();
        DifyStream {
            lines: Lines::with_max_line_bytes(
                HttpResponse { response: response.into(), permit: None }, MAX_EVENT_BYTES + 6,
            ).unwrap(),
            request_id: "request".into(), index: 0, finished: false, data: String::new(),
            metadata: Map::new(), agent_answer: String::new(), saw_agent: false,
            workflow: false, message_end: false,
        }
    }

    #[tokio::test]
    async fn dify_accepts_an_exact_event_budget_with_the_sse_prefix() {
        let empty = json!({"event":"message","answer":""}).to_string();
        let answer = "x".repeat(MAX_EVENT_BYTES - empty.len());
        let event = json!({"event":"message","answer":answer}).to_string();
        assert_eq!(event.len(), MAX_EVENT_BYTES);
        let mut stream = fixture(format!("data: {event}\n\n").into());
        let chunk = stream.next().await.unwrap().unwrap();
        assert!(!chunk.is_final);
        assert_eq!(data_value(&chunk.content), json!(answer));
        Box::new(stream).close().await.unwrap();
    }

    #[tokio::test]
    async fn oversized_unfinished_dify_line_fails_at_its_own_budget_without_eof() {
        let mut chunk = Some(vec![b'x'; MAX_EVENT_BYTES + 7]);
        let body = reqwest::Body::wrap_stream(futures_util::stream::poll_fn(move |_| {
            match chunk.take() {
                Some(bytes) => std::task::Poll::Ready(Some(Ok::<_, std::io::Error>(bytes))),
                None => std::task::Poll::Pending,
            }
        }));
        let mut stream = fixture(body);
        let error = stream.next().now_or_never().expect("must not wait for EOF").unwrap_err();
        assert!(matches!(error, DispatchError::AdapterValidationFailed("provider_stream_line_too_large")));
        assert!(stream.finished);
        assert!(!stream.message_end);
        assert!(stream.data.is_empty());
        assert!(stream.next().await.unwrap().is_none());
    }

    fn multiline_event(extra_bytes: usize) -> (reqwest::Body, String) {
        let first = "{\"event\":\"message\",\"answer\":";
        let second_overhead = "\"\"}".len();
        let answer = "x".repeat(MAX_EVENT_BYTES - first.len() - 1 - second_overhead + extra_bytes);
        let second = format!("\"{answer}\"}}");
        // Neither physical line is over budget. The inserted SSE separator is
        // valid JSON whitespace and must count toward the assembled event cap.
        assert!(second.len() < MAX_EVENT_BYTES);
        assert_eq!(first.len() + 1 + second.len(), MAX_EVENT_BYTES + extra_bytes);
        (format!("data: {first}\ndata: {second}\n\n").into(), answer)
    }

    #[tokio::test]
    async fn dify_accepts_multiline_data_exactly_at_its_assembled_event_budget() {
        let (body, answer) = multiline_event(0);
        let mut stream = fixture(body);
        let chunk = stream.next().await.unwrap().unwrap();
        assert!(!chunk.is_final);
        assert_eq!(data_value(&chunk.content), json!(answer));
        Box::new(stream).close().await.unwrap();
    }

    #[tokio::test]
    async fn dify_rejects_multiline_data_when_the_separator_exceeds_its_event_budget() {
        let (body, _) = multiline_event(1);
        let mut stream = fixture(body);
        let error = stream.next().await.unwrap_err();
        assert!(matches!(error, DispatchError::AdapterValidationFailed("dify_stream_event_too_large")));
        assert!(stream.finished);
        assert!(!stream.message_end);
        assert!(stream.data.is_empty());
        assert!(stream.next().await.unwrap().is_none());
    }
    #[tokio::test]
    async fn dify_history_preserves_local_terminal_observation() {
        use ai_platform_gateway_foundation::{dispatcher::StreamStatus, stream_statistics::StreamStatistics};
        let mut receipts = Vec::new();
        for (name, tail, expected) in [
            ("success", vec![json!({"event":"message_end"}), json!({"event":"workflow_finished","data":{"status":"succeeded","outputs":{"answer":"partial answer"}}})], "completed"),
            ("workflow_failed", vec![json!({"event":"message_end"}), json!({"event":"workflow_finished","data":{"status":"failed","error":"fixture failure"}})], "failed"),
            ("provider_error", vec![json!({"event":"error","message":"fixture failure"})], "failed"),
            ("truncated", vec![], "failed"),
            ("local_close", vec![], "interrupted"),
        ] {
            let mut events = vec![json!({"event":"workflow_started"}), json!({"event":"message","answer":"partial answer","conversation_id":"fixture-conversation"})];
            events.extend(tail);
            let wire = events.iter().map(|event| format!("data: {event}\n\n")).collect::<String>();
            let mut producer = fixture(wire.into());
            let mut stats = StreamStatistics::default();
            let mut status = StreamStatus::Success;
            let mut observed_terminal = false;
            loop {
                match producer.next().await {
                    Ok(Some(chunk)) => {
                        observed_terminal |= chunk.is_final;
                        stats.observe(chunk, 1);
                        if name == "local_close" { status = StreamStatus::Unknown; break; }
                    }
                    Ok(None) => break,
                    Err(_) => { status = StreamStatus::Error; break; }
                }
            }
            Box::new(producer).close().await.unwrap();
            assert_eq!(observed_terminal, name == "success", "{name}");
            let finalization = stats.finalize(status, "fixture input", 3, Some(4)).unwrap();
            assert_eq!(finalization.output_text, "partial answer", "{name}");
            let metadata = finalization.session_metadata.unwrap();
            receipts.push(json!({"name":name,"expected":expected,"upstream_terminal":observed_terminal,
                "history":[{"role":"assistant","content":finalization.output_text,"metadata":metadata}]}));
        }
        // Test-only optional receipt captures actual production parser/statistics
        // output; no provider invocation, listener, or synthetic artifact URL.
        if let Ok(path) = std::env::var("DIFY_HISTORY_RECEIPT") {
            std::fs::write(path, serde_json::to_vec_pretty(&receipts).unwrap()).unwrap();
        }
        for receipt in receipts {
            assert_eq!(receipt["history"][0]["metadata"]["gateway_stream_status"], receipt["expected"], "{}", receipt["name"]);
        }
    }

    #[tokio::test]
    async fn dify_history_empty_structured_and_file_outputs_remain_text_only() {
        use ai_platform_gateway_foundation::{dispatcher::StreamStatus, stream_statistics::StreamStatistics};
        for outputs in [json!({}), json!({"summary":{"score":95}}), json!({"files":[{"type":"document","url":"https://example.invalid/fixture.pdf"}]})] {
            let events = [json!({"event":"workflow_started"}), json!({"event":"message_end"}), json!({"event":"workflow_finished","data":{"status":"succeeded","outputs":outputs}})];
            let wire = events.iter().map(|event| format!("data: {event}\n\n")).collect::<String>();
            let mut producer = fixture(wire.into());
            let mut stats = StreamStatistics::default();
            let mut finals = 0;
            while let Some(chunk) = producer.next().await.unwrap() { finals += usize::from(chunk.is_final); stats.observe(chunk, 1); }
            assert_eq!(finals, 1);
            let result = stats.finalize(StreamStatus::Success, "input", 3, Some(4)).unwrap();
            assert!(result.output_text.is_empty());
            assert!(result.session_metadata.is_none());
        }
    }
}
