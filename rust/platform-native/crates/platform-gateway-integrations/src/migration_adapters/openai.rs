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
        let embedding = service.service_type == ServiceType::Embedding;
        let embedding_input = if embedding {
            if texts.len() == 1 {
                json!(texts[0].clone())
            } else {
                json!(texts.clone())
            }
        } else {
            Value::Null
        };
        let mut payload = if embedding {
            json!({"model":model,"input":embedding_input})
        } else {
            json!({"model":model,"messages":[{"role":"user","content":texts.join("\n")} ]})
        };
        if stream {
            payload["stream"] = json!(true);
        }
        for (k, v) in params {
            if k != "model" && !(embedding && k == "input") {
                payload[k] = v;
            }
        }
        if stream && payload.get("stream_options").is_none() {
            payload["stream_options"] = json!({"include_usage":true});
        }
        Ok(payload)
    }
}
fn embedding_result(request_id: &str, response: Value) -> UnifiedResponse {
    let usage = response.get("usage").and_then(Value::as_object).cloned();
    let mut result = UnifiedResponse::new(
        request_id,
        "success",
        vec![content(ContentType::Json, response)],
    );
    result.usage = usage;
    result
}

fn safe_upstream_error_code(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let code = value
        .pointer("/error/code")
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)?;
    if code.is_empty()
        || code.len() > 64
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
    {
        return None;
    }
    Some(code.to_owned())
}

const MAX_UPSTREAM_ERROR_DIAGNOSTIC_BYTES: usize = 4096;

async fn require_successful_upstream(response: HttpResponse) -> Result<HttpResponse, AdapterError> {
    let status = response.response.status();
    if status.is_success() {
        return Ok(response);
    }

    let HttpResponse {
        mut response,
        permit,
    } = response;
    let mut bytes = Vec::with_capacity(MAX_UPSTREAM_ERROR_DIAGNOSTIC_BYTES);
    while bytes.len() < MAX_UPSTREAM_ERROR_DIAGNOSTIC_BYTES {
        let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| super::transport::transport_error(&error, true))?
        else {
            break;
        };
        let take = (MAX_UPSTREAM_ERROR_DIAGNOSTIC_BYTES - bytes.len()).min(chunk.len());
        bytes.extend_from_slice(&chunk[..take]);
        if take < chunk.len() {
            break;
        }
    }
    drop(response);
    drop(permit);
    let error_code = safe_upstream_error_code(&bytes).unwrap_or_else(|| "unclassified".into());
    tracing::warn!(
        target: "ai_platform_gateway::openai_upstream",
        provider_status = status.as_u16(),
        provider_error_code = %error_code,
        "OpenAI-compatible provider rejected the request"
    );
    Err(AdapterFailure::HttpStatus(status.as_u16()).into())
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
            let response = require_successful_upstream(response).await?;
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
                return Ok(embedding_result(&request.request_id, response));
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
            let response = require_successful_upstream(response).await?;
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

    #[test]
    fn embedding_provider_usage_is_preserved_for_gateway_billing() {
        let result = embedding_result(
            "request",
            json!({
                "object":"list",
                "data":[{"object":"embedding","index":0,"embedding":[0.1,0.2]}],
                "usage":{"prompt_tokens":3,"total_tokens":3}
            }),
        );
        assert_eq!(result.usage.unwrap()["prompt_tokens"], 3);
        assert_eq!(
            result.outputs[0].data,
            Some(ContentData::Json(json!({
                "object":"list",
                "data":[{"object":"embedding","index":0,"embedding":[0.1,0.2]}],
                "usage":{"prompt_tokens":3,"total_tokens":3}
            })))
        );
    }

    #[test]
    fn embedding_payload_preserves_batch_items_for_provider_and_billing_parity() {
        let mut definition = crate::protocol::ServiceDefinition::new("embedding", "Embedding");
        definition.service_type = ServiceType::Embedding;
        definition.metadata = json!({"model":"text-embedding-3-small"});
        let adapter = OpenAiAdapter {
            http: HttpTransport::new(ServiceHandle::new(definition.clone())).unwrap(),
            service: ServiceHandle::new(definition),
        };
        let request = UnifiedRequest {
            request_id: "r".into(),
            service_id: "embedding".into(),
            inputs: vec![ContentItem::text("one"), ContentItem::text("two")],
            session_id: None,
            context: None,
            parameters: Some(Map::from_iter([
                ("input".into(), json!(["one", "two"])),
                ("dimensions".into(), json!(32)),
            ])),
            callback_url: None,
            priority: 0,
            user_id: "u".into(),
            tenant_id: "t".into(),
            timestamp: crate::protocol::ProtocolTimestamp::now(),
        };
        let payload = adapter.payload(&request, false).unwrap();
        assert_eq!(payload["input"], json!(["one", "two"]));
        assert_eq!(payload["model"], "text-embedding-3-small");
        assert_eq!(payload["dimensions"], 32);
    }

    #[test]
    fn upstream_error_diagnostic_keeps_only_a_bounded_static_code() {
        assert_eq!(
            safe_upstream_error_code(
                br#"{"error":{"code":"invalid_parameter","message":"private body"}}"#
            ),
            Some("invalid_parameter".into())
        );
        assert_eq!(
            safe_upstream_error_code(br#"{"code":"bad:request-v2_1"}"#),
            Some("bad:request-v2_1".into())
        );
        assert_eq!(
            safe_upstream_error_code(br#"{"error":{"code":"secret key abc"}}"#),
            None
        );
        assert_eq!(
            safe_upstream_error_code(format!(r#"{{"code":"{}"}}"#, "a".repeat(65)).as_bytes()),
            None
        );
        assert_eq!(safe_upstream_error_code(b"not-json"), None);
    }

    #[tokio::test]
    async fn oversized_unfinished_line_terminates_openai_without_a_final_chunk() {
        let mut chunk = Some(vec![b'x'; 4 * 1024 * 1024 + 1]);
        let body = reqwest::Body::wrap_stream(futures_util::stream::poll_fn(move |_| match chunk
            .take()
        {
            Some(bytes) => std::task::Poll::Ready(Some(Ok::<_, std::io::Error>(bytes))),
            None => std::task::Poll::Pending,
        }));
        let response = axum::http::Response::builder()
            .status(200)
            .body(body)
            .unwrap();
        let mut stream = OpenAiStream {
            lines: Lines::new(HttpResponse {
                response: response.into(),
                permit: None,
            })
            .unwrap(),
            request_id: "request".into(),
            index: 0,
            usage: None,
            terminal: false,
            finished: false,
        };
        let error = stream
            .next()
            .now_or_never()
            .expect("must not wait for EOF")
            .unwrap_err();
        assert!(matches!(
            error,
            DispatchError::AdapterValidationFailed("provider_stream_line_too_large")
        ));
        assert!(stream.finished);
        assert!(!stream.terminal);
        assert!(stream.next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn embedding_adapter_dispatch_and_usage_accounting_accept_base64_input_only_usage() {
        use ai_platform_gateway_foundation::{
            capacity::CapacityResolver,
            capacity_admission::{AdmissionOptions, CapacityAdmissionController},
            dispatcher::{DispatchContext, DispatchExecutor},
            gateway_dispatcher::usage_event,
            protocol::{ContentItem, TransportType},
        };
        use axum::{Json, Router, routing::post};
        use serde_json::Map;
        use std::sync::{Arc, Mutex};

        let provider_response = json!({
            "object":"list",
            "data":[{"object":"embedding","index":0,"embedding":"AACAPwAAAEA="}],
            "usage":{"prompt_tokens":5,"total_tokens":5}
        });
        let observed: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        let route_observed = observed.clone();
        let response = provider_response.clone();
        let app = Router::new().route(
            "/v1/embeddings",
            post(move |Json(body): Json<Value>| {
                let observed = route_observed.clone();
                let response = response.clone();
                async move {
                    *observed.lock().unwrap() = Some(body);
                    Json(response)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let service_id = format!("managed-openai-{}", "a".repeat(48));
        let mut definition =
            crate::protocol::ServiceDefinition::new(service_id.clone(), "Embedding");
        definition.service_type = ServiceType::Embedding;
        definition.connector_type = TransportType::Http;
        definition.connector_config = json!({"base_url":format!("http://{address}")});
        definition.metadata = json!({
            "adapter_type":"openai",
            "model":"text-embedding-v4",
            "provider":"dashscope"
        });
        definition.accepted_content_types = vec![ContentType::Text];
        definition.output_content_types = vec![ContentType::Json];
        let service = ServiceHandle::new(definition.clone());
        let adapter = Arc::new(OpenAiAdapter {
            http: HttpTransport::new(service.clone()).unwrap(),
            service: service.clone(),
        });
        let dispatch_service = service.snapshot().dispatch_view().unwrap();
        let admission =
            Arc::new(CapacityAdmissionController::new(AdmissionOptions::default(), None).unwrap());
        let executor = DispatchExecutor::new(admission, CapacityResolver::default());

        let input = "OpenAI-compatible embedding smoke test.";
        let mut request = UnifiedRequest::new(
            "embedding-request",
            "embedding",
            vec![ContentItem::text(input)],
        );
        request.user_id = "user".into();
        request.tenant_id = "tenant".into();
        request.parameters = Some(Map::from_iter([
            ("input".into(), json!(input)),
            ("encoding_format".into(), json!("base64")),
        ]));
        let context = DispatchContext {
            request_id: request.request_id.clone(),
            tenant_id: request.tenant_id.clone(),
            user_id: request.user_id.clone(),
        };
        let response = executor
            .invoke_adapter(&context, &dispatch_service, adapter, request.clone())
            .await
            .unwrap();

        let sent = observed.lock().unwrap().clone().unwrap();
        assert_eq!(sent["model"], "text-embedding-v4");
        assert_eq!(sent["input"], input);
        assert_eq!(sent["encoding_format"], "base64");
        assert_eq!(response.usage.as_ref().unwrap()["prompt_tokens"], json!(5));
        assert_eq!(response.usage.as_ref().unwrap()["total_tokens"], json!(5));
        assert_eq!(
            response.outputs[0].data,
            Some(ContentData::Json(provider_response))
        );

        let usage_payload = json!({"usage":response.usage.unwrap()});
        let record = usage_event(
            &request,
            &service.snapshot(),
            &usage_payload,
            3,
            "invoke",
            "success",
            "",
        )
        .unwrap();
        assert_eq!(record.input_tokens, 5);
        assert_eq!(record.output_tokens, 0);
        assert_eq!(record.metadata["token_source"], "upstream");
        assert_eq!(record.service_id, service_id);
        assert!(record.service_id.len() <= 64);

        server.abort();
    }
}
