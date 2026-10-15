use super::*;
use reqwest::header::HeaderMap;
pub struct OpenAiAdapter {
    pub http: HttpTransport,
    pub service: ServiceHandle,
}
impl OpenAiAdapter {
    fn payload(&self, request: &UnifiedRequest, stream: bool) -> Result<Value, AdapterError> {
        let texts: Vec<String> = request
            .inputs
            .iter()
            .filter(|i| i.content_type == ContentType::Text)
            .map(data_value)
            .filter(python_truthy)
            .map(|v| pystr(&v))
            .collect();
        if texts.is_empty() {
            return Err(validation("text input is required"));
        }
        let params = request.parameters.clone().unwrap_or_default();
        let service = self.service.snapshot();
        let model = params
            .get("model")
            .filter(|v| python_truthy(v))
            .or_else(|| service.metadata.get("model").filter(|v| python_truthy(v)))
            .cloned()
            .unwrap_or(json!("gpt-4o-mini"));
        let mut payload = if service.service_type == ServiceType::Embedding {
            json!({"model":model,"input":texts.join("\n")})
        } else {
            json!({"model":model,"messages":[{"role":"user","content":texts.join("\n")} ]})
        };
        if stream {
            payload["stream"] = json!(true);
        }
        for (k, v) in params {
            if k != "model" {
                payload[k] = v;
            }
        }
        if stream && payload.get("stream_options").is_none() {
            payload["stream_options"] = json!({"include_usage":true});
        }
        Ok(payload)
    }
}
impl ProtocolAdapter for OpenAiAdapter {
    fn invoke<'a>(&'a self, request: &'a UnifiedRequest) -> AdapterFuture<'a, UnifiedResponse> {
        Box::pin(async move {
            let embedding = self.service.snapshot().service_type == ServiceType::Embedding;
            let response = self
                .http
                .post_json(
                    if embedding {
                        "/v1/embeddings"
                    } else {
                        "/v1/chat/completions"
                    },
                    &self.payload(request, false)?,
                    HeaderMap::new(),
                )
                .await?;
            let status = response.response.status();
            let (is_json, bytes) = response.body().await?;
            let response: Value = if is_json {
                serde_json::from_slice(&bytes)
                    .map_err(|_| validation("provider_response_json_invalid"))?
            } else if embedding {
                return Err(validation("embedding response is not JSON"));
            } else {
                json!(format!(
                    "<Response [{} {}]>",
                    status.as_u16(),
                    status.canonical_reason().unwrap_or("")
                ))
            };
            if embedding {
                return Ok(UnifiedResponse::new(
                    &request.request_id,
                    "success",
                    vec![content(ContentType::Json, response)],
                ));
            }
            let text = if response.is_object() {
                response
                    .get("choices")
                    .filter(|v| python_truthy(v))
                    .and_then(|v| v.get(0))
                    .and_then(|v| v.get("message"))
                    .and_then(|v| v.get("content"))
                    .filter(|v| python_truthy(v))
                    .map(pystr)
                    .unwrap_or_default()
            } else {
                pystr(&response)
            };
            let mut result = UnifiedResponse::new(
                &request.request_id,
                "success",
                vec![content(ContentType::Text, json!(text))],
            );
            result.usage = response.get("usage").and_then(Value::as_object).cloned();
            Ok(result)
        })
    }
    fn stream(
        &self,
        request: UnifiedRequest,
    ) -> AdapterFuture<'_, Box<dyn DispatchProducer<StreamChunk>>> {
        Box::pin(async move {
            if self.service.snapshot().service_type == ServiceType::Embedding {
                return Err(validation("stream not supported for embedding"));
            }
            let response = self
                .http
                .post_json(
                    "/v1/chat/completions",
                    &self.payload(&request, true)?,
                    HeaderMap::new(),
                )
                .await?;
            Ok(Box::new(OpenAiStream {
                lines: Lines::new(response)?,
                request_id: request.request_id,
                index: 0,
                usage: None,
                terminal: false,
                finished: false,
            }) as Box<dyn DispatchProducer<StreamChunk>>)
        })
    }
    fn health_check(&self) -> AdapterFuture<'_, bool> {
        Box::pin(async { Ok(self.http.health().await) })
    }
}
struct OpenAiStream {
    lines: Lines,
    request_id: String,
    index: i64,
    usage: Option<Map<String, Value>>,
    terminal: bool,
    finished: bool,
}
impl OpenAiStream {
    async fn read(&mut self) -> Result<Option<StreamChunk>, AdapterError> {
        if self.finished {
            return Ok(None);
        }
        while let Some(line) = self.lines.next().await? {
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                self.terminal = true;
                break;
            }
            let event: Value = serde_json::from_str(data)
                .map_err(|_| validation("provider_stream_json_invalid"))?;
            if let Some(usage) = event.get("usage").and_then(Value::as_object) {
                self.usage = Some(usage.clone());
            }
            if !event.is_object() || event.get("error").is_some_and(python_truthy) {
                return Err(validation("provider_stream_error"));
            }
            let empty = Vec::new();
            let choices = event
                .get("choices")
                .filter(|v| python_truthy(v))
                .map(|v| {
                    v.as_array()
                        .ok_or_else(|| validation("provider_choices_invalid"))
                })
                .transpose()?
                .unwrap_or(&empty);
            let Some(choice) = choices.first() else {
                continue;
            };
            if !choice.is_object() || choice.get("delta").is_some_and(|v| !v.is_object()) {
                return Err(validation("provider_delta_invalid"));
            }
            if let Some(finish) = choice.get("finish_reason").filter(|v| !v.is_null()) {
                if !matches!(
                    finish.as_str(),
                    Some("stop" | "tool_calls" | "function_call")
                ) {
                    return Err(validation("provider_stream_incomplete"));
                }
                self.terminal = true;
            }
            if let Some(delta) = choice
                .get("delta")
                .and_then(|v| v.get("content"))
                .filter(|v| python_truthy(v))
            {
                let chunk = chunk(
                    &self.request_id,
                    self.index,
                    delta.clone(),
                    StreamEventType::TextDelta,
                    false,
                );
                self.index += 1;
                return Ok(Some(chunk));
            }
        }
        self.lines.close();
        self.finished = true;
        if !self.terminal {
            return Err(validation("provider_stream_incomplete"));
        }
        let mut result = chunk(
            &self.request_id,
            self.index,
            json!(""),
            StreamEventType::Final,
            true,
        );
        if let Some(usage) = self.usage.take().filter(|v| !v.is_empty()) {
            result.metadata = Some(Map::from_iter([("usage".into(), Value::Object(usage))]));
        }
        Ok(Some(result))
    }
}
impl DispatchProducer<StreamChunk> for OpenAiStream {
    fn next(&mut self) -> DispatchFuture<'_, Option<StreamChunk>> {
        Box::pin(async move {
            match self.read().await {
                Ok(v) => Ok(v),
                Err(e) => {
                    self.lines.close();
                    self.finished = true;
                    Err(dispatch(e))
                }
            }
        })
    }
    fn close(mut self: Box<Self>) -> DispatchFuture<'static, ()> {
        Box::pin(async move {
            self.lines.close();
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    #[tokio::test]
    async fn oversized_unfinished_line_terminates_openai_without_a_final_chunk() {
        let mut chunk = Some(vec![b'x'; 4 * 1024 * 1024 + 1]);
        let body = reqwest::Body::wrap_stream(futures_util::stream::poll_fn(move |_| {
            match chunk.take() {
                Some(bytes) => std::task::Poll::Ready(Some(Ok::<_, std::io::Error>(bytes))),
                None => std::task::Poll::Pending,
            }
        }));
        let response = axum::http::Response::builder().status(200).body(body).unwrap();
        let mut stream = OpenAiStream {
            lines: Lines::new(HttpResponse { response: response.into(), permit: None }).unwrap(),
            request_id: "request".into(), index: 0, usage: None, terminal: false, finished: false,
        };
        let error = stream.next().now_or_never().expect("must not wait for EOF").unwrap_err();
        assert!(matches!(error, DispatchError::AdapterValidationFailed("provider_stream_line_too_large")));
        assert!(stream.finished);
        assert!(!stream.terminal);
        assert!(stream.next().await.unwrap().is_none());
    }
}
