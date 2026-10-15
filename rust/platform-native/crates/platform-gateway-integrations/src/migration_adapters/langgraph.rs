//! Remote LangGraph service protocol, distinct from the transparent REST router.
//! Session cache, payload scoping, SSE text/tool projections and final usage follow
//! src/adapters/langgraph.py. Python module loading is intentionally never
//! attempted here; in-process graphs require an explicit native adapter.
use super::*;
use crate::protocol::ToolCall;
use redis::AsyncCommands;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Mutex, OnceLock},
};
#[derive(Clone, Default)]
pub struct LangGraphAdapterOptions {
    pub redis: Option<redis::aio::ConnectionManager>,
    pub prepare: Option<Arc<dyn LangGraphPayloadPreparation>>,
    pub callback_allowlist: Vec<String>,
}
pub trait LangGraphPayloadPreparation: Send + Sync {
    fn prepare<'a>(
        &'a self,
        request: &'a UnifiedRequest,
        service: &'a ServiceHandle,
        path: &'a str,
        payload: Value,
    ) -> AdapterFuture<'a, Value>;
}
pub struct LangGraphAdapterFactory {
    pub options: LangGraphAdapterOptions,
}
impl AdapterFactory for LangGraphAdapterFactory {
    fn create(&self, service: ServiceHandle) -> Result<Arc<dyn ProtocolAdapter>, RegistryError> {
        Ok(Arc::new(LangGraphAdapter::with_options(
            service,
            self.options.clone(),
        )?))
    }
}
pub struct LangGraphAdapter {
    http: HttpTransport,
    stream_http: HttpTransport,
    service: ServiceHandle,
    config: crate::service_ownership::ConnectorHandle,
    assistant: Value,
    options: LangGraphAdapterOptions,
}
static THREADS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
impl LangGraphAdapter {
    pub fn new(service: ServiceHandle) -> Result<Self, RegistryError> {
        Self::with_options(service, LangGraphAdapterOptions::default())
    }
    pub fn with_options(
        service: ServiceHandle,
        options: LangGraphAdapterOptions,
    ) -> Result<Self, RegistryError> {
        let config = service.connector().dictionary_or_empty();
        let snapshot = config.snapshot();
        let assistant = ["assistant_id", "graph_id", "graph_name"]
            .iter()
            .find_map(|k| snapshot.get(k).filter(|v| python_truthy(v)))
            .cloned()
            .unwrap_or(json!(service.service_id()));
        let http = HttpTransport::new(service.clone())?;
        let stream_http = http.langgraph_stream_variant()?;
        Ok(Self {
            http,
            stream_http,
            service,
            config,
            assistant,
            options,
        })
    }
    fn headers(&self, request: &UnifiedRequest) -> Result<HeaderMap, AdapterError> {
        let config = self.config.snapshot();
        let mut headers = HeaderMap::new();
        if config
            .get("forward_auth")
            .is_some_and(|v| !python_truthy(v))
        {
            return Ok(headers);
        }
        let user = config
            .get("auth_user_id")
            .filter(|v| python_truthy(v))
            .map(pystr)
            .unwrap_or_else(|| {
                if request.user_id.is_empty() {
                    "gateway-user".into()
                } else {
                    request.user_id.clone()
                }
            });
        let user_type = config
            .get("auth_user_type")
            .map(pystr)
            .unwrap_or("user".into());
        let name = if request.user_id.is_empty() {
            "Gateway User".into()
        } else {
            format!("User-{}", request.user_id)
        };
        let mut fields = vec![
            ("x-user-id", user.clone()),
            ("x-user-type", user_type),
            ("x-user-name", name),
            ("x-user-permissions", "read,write".into()),
            ("authorization", format!("Bearer gateway-{user}")),
        ];
        if !request.tenant_id.is_empty() {
            fields.push(("x-tenant-id", request.tenant_id.clone()));
        }
        for (k, v) in fields {
            let mut value = HeaderValue::from_str(&v)
                .map_err(|_| validation("invalid LangGraph auth header"))?;
            value.set_sensitive(true);
            headers.insert(HeaderName::from_static(k), value);
        }
        Ok(headers)
    }
    fn run_config(&self, request: &UnifiedRequest, thread: Option<&str>) -> Value {
        let mut run = request
            .parameters
            .as_ref()
            .and_then(|p| p.get("config"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut configurable = run
            .get("configurable")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        configurable.remove("gateway_model");
        for source in [&request.context, &request.parameters]
            .into_iter()
            .flatten()
        {
            if let Some(values) = source.get("configurable").and_then(Value::as_object) {
                for (k, v) in values {
                    if k != "gateway_model" && !v.is_null() {
                        configurable.entry(k.clone()).or_insert(v.clone());
                    }
                }
            }
            for k in ["dataset_id", "gateway_token"] {
                if let Some(v) = source.get(k).filter(|v| !v.is_null()) {
                    configurable.entry(k).or_insert(v.clone());
                }
            }
        }
        configurable.insert("user_id".into(), json!(request.user_id));
        configurable.insert("tenant_id".into(), json!(request.tenant_id));
        configurable.insert("checkpoint_ns".into(), json!(request.tenant_id));
        configurable.insert(
            "thread_id".into(),
            json!(
                thread
                    .or(request.session_id.as_deref().filter(|s| !s.is_empty()))
                    .unwrap_or(&request.request_id)
            ),
        );
        run.insert("configurable".into(), json!(configurable));
        json!(run)
    }
    async fn ensure_thread(
        &self,
        session: &str,
        request: &UnifiedRequest,
    ) -> Result<String, AdapterError> {
        self.ensure_thread_with(session, request, |builder| self.http.send(builder))
            .await
    }
    // One private I/O seam keeps offline tests on the production admission/cache path.
    async fn ensure_thread_with<F, Fut>(
        &self,
        session: &str,
        request: &UnifiedRequest,
        send: F,
    ) -> Result<String, AdapterError>
    where
        F: Fn(reqwest::RequestBuilder) -> Fut,
        Fut: std::future::Future<Output = Result<HttpResponse, AdapterError>>,
    {
        let cache = THREADS.get_or_init(|| Mutex::new(HashMap::new()));
        if let Some(id) = cache
            .lock()
            .expect("thread cache poisoned")
            .get(session)
            .cloned()
        {
            return Ok(id);
        }
        let mut redis = self.options.redis.clone();
        let key = format!("lg:thread_map:{session}");
        if let Some(redis) = redis.as_mut() {
            if let Ok(Some(id)) = redis.get::<_, Option<String>>(&key).await
                && !id.is_empty()
            {
                cache
                    .lock()
                    .expect("thread cache poisoned")
                    .insert(session.into(), id.clone());
                return Ok(id);
            }
        }
        let id = if uuid::Uuid::parse_str(session).is_ok() {
            session.into()
        } else {
            uuid::Uuid::new_v4().to_string()
        };
        let headers = self
            .headers(request)
            .map_err(|_| validation("langgraph_thread_creation_failed"))?;
        let payload = json!({
            "thread_id": id,
            "if_exists": "do_nothing",
            "metadata": {
                "user_id": request.user_id,
                "tenant_id": request.tenant_id,
                "original_session_id": session,
            },
        });
        let response = send(
            self.http
                .client
                .post(
                    self.http
                        .url("/threads")
                        .map_err(|_| validation("langgraph_thread_creation_failed"))?,
                )
                .headers(headers.clone())
                .json(&payload),
        )
        .await
        .map_err(|_| validation("langgraph_thread_creation_failed"))?;
        let (response, failure) = if response.response.status().as_u16() == 409 {
            // Release the conflict response/permit before the one exact-ID readback.
            drop(response);
            let response = send(
                self.http
                    .client
                    .get(
                        self.http
                            .url(&format!("/threads/{id}"))
                            .map_err(|_| validation("langgraph_thread_readback_failed"))?,
                    )
                    .headers(headers),
            )
            .await
            .map_err(|_| validation("langgraph_thread_readback_failed"))?;
            (response, "langgraph_thread_readback_failed")
        } else {
            (response, "langgraph_thread_creation_failed")
        };
        response.check_status().map_err(|_| validation(failure))?;
        let (is_json, bytes) = response.body().await.map_err(|_| validation(failure))?;
        let created: Value = if is_json {
            serde_json::from_slice(&bytes).map_err(|_| validation(failure))?
        } else {
            return Err(validation("langgraph_thread_identity_unconfirmed"));
        };
        let matches = created
            .as_object()
            .filter(|v| !v.contains_key("__error__"))
            .and_then(|v| v.get("thread_id"))
            .and_then(Value::as_str)
            .and_then(|returned| uuid::Uuid::parse_str(returned).ok())
            .is_some_and(|returned| Some(returned) == uuid::Uuid::parse_str(&id).ok());
        if !matches {
            return Err(validation("langgraph_thread_identity_unconfirmed"));
        }
        // Only an acknowledged thread may reach cache publication or model dispatch.
        cache
            .lock()
            .expect("thread cache poisoned")
            .insert(session.into(), id.clone());
        if let Some(redis) = redis.as_mut() {
            let _: Result<(), _> = redis.set_ex(&key, &id, 604800).await;
        }
        Ok(id)
    }
    async fn payload(
        &self,
        request: &UnifiedRequest,
        stream: bool,
    ) -> Result<(String, Value), AdapterError> {
        let texts: Vec<_> = request
            .inputs
            .iter()
            .filter(|i| i.content_type == ContentType::Text)
            .map(data_value)
            .filter(python_truthy)
            .map(|v| pystr(&v))
            .collect();
        if texts.is_empty() {
            return Err(validation("text input required"));
        }
        let params = request.parameters.clone().unwrap_or_default();
        let config = self.config.snapshot();
        let mut input = json!({"messages":[{"role":"user","content":texts.join("\n")}]});
        if let Some(extra) = params.get("input").and_then(Value::as_object) {
            input.as_object_mut().expect("object").extend(extra.clone());
        }
        // Reject an invalid callback before creating or caching a remote thread.
        let callback = request.callback_url.as_deref().filter(|s| !s.is_empty());
        if !stream && let Some(callback) = callback {
            validate_callback(callback, &self.options.callback_allowlist).await?;
        }
        let thread = if self.service.snapshot().session_enabled {
            if let Some(session) = request.session_id.as_deref().filter(|s| !s.is_empty()) {
                Some(self.ensure_thread(session, request).await?)
            } else {
                None
            }
        } else {
            None
        };
        let (key, default, path) = match (stream, thread.as_deref()) {
            (false, None) => ("invoke_endpoint", "/runs/wait".into(), "runs/wait".into()),
            (true, None) => (
                "stream_endpoint",
                "/runs/stream".into(),
                "runs/stream".into(),
            ),
            (false, Some(id)) => (
                "thread_invoke_endpoint",
                format!("/threads/{id}/runs/wait"),
                format!("threads/{id}/runs/wait"),
            ),
            (true, Some(id)) => (
                "thread_stream_endpoint",
                format!("/threads/{id}/runs/stream"),
                format!("threads/{id}/runs/stream"),
            ),
        };
        let endpoint = config
            .get(key)
            .and_then(Value::as_str)
            .map(|s| s.replace("{thread_id}", thread.as_deref().unwrap_or("")))
            .unwrap_or(default);
        let mut payload = json!({"assistant_id":self.assistant,"input":input,"metadata":{"request_id":request.request_id,"user_id":request.user_id,"tenant_id":request.tenant_id},"config":self.run_config(request,thread.as_deref())});
        if stream {
            let mut mode = params
                .get("stream_mode")
                .filter(|v| !v.is_null())
                .or_else(|| config.get("stream_mode").filter(|v| !v.is_null()))
                .cloned()
                .unwrap_or(json!(["messages", "updates"]));
            if let Some(s) = mode.as_str().filter(|s| s.contains(',')) {
                mode = json!(
                    s.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                );
            }
            payload["stream_mode"] = mode;
            if let Some(sub) = params
                .get("stream_subgraphs")
                .filter(|v| !v.is_null())
                .or_else(|| config.get("stream_subgraphs").filter(|v| !v.is_null()))
            {
                payload["stream_subgraphs"] = sub.clone();
            }
        } else if let Some(callback) = callback {
            payload["webhook"] = json!(callback);
        }
        if let Some(prepare) = &self.options.prepare {
            payload = prepare
                .prepare(request, &self.service, &path, payload)
                .await?;
        } else if config
            .get("model_override")
            .and_then(|v| v.get("enabled"))
            .is_some_and(python_truthy)
        {
            return Err(validation(
                "LangGraph model control plane is not initialized",
            ));
        }
        Ok((endpoint, payload))
    }
}
async fn validate_callback(callback: &str, allowlist: &[String]) -> Result<(), AdapterError> {
    let url =
        reqwest::Url::parse(callback).map_err(|_| validation("callback_url is not allowed"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(validation("callback_url is not allowed"));
    }
    let host = url
        .host_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| validation("callback_url is not allowed"))?
        .to_lowercase();
    let host = host.trim_end_matches('.');
    if allowlist
        .iter()
        .map(String::as_str)
        .map(|s| s.trim().trim_start_matches('.').to_lowercase())
        .filter(|s| !s.is_empty())
        .any(|s| host == s || host.ends_with(&format!(".{s}")))
    {
        return Ok(());
    }
    super::media::validated_destination(callback)
        .await
        .map(|_| ())
        .map_err(|_| validation("callback_url is not allowed"))
}
fn scrub_detail(value: &str) -> String {
    static URLS: OnceLock<regex::Regex> = OnceLock::new();
    let text = crate::metrics_redaction::redact_sensitive_text(value);
    URLS.get_or_init(|| {
        regex::Regex::new(r#"(?i)\b(?:https?|wss?)://[^\s"'<>]+"#).expect("URL redaction pattern")
    })
    .replace_all(&text, "[upstream URL]")
    .into_owned()
}
fn validation_detail(value: String) -> AdapterError {
    DispatchError::AdapterValidationDetail(scrub_detail(&value)).into()
}
fn display_endpoint(endpoint: &str) -> String {
    scrub_detail(endpoint.split(['?', '#']).next().unwrap_or_default())
}
fn request_error(operation: &str, endpoint: &str, error: AdapterError) -> AdapterError {
    let AdapterError::Dispatch(DispatchError::Adapter(failure)) = &error else {
        return error;
    };
    // Transport diagnostics are classified rather than exposing reqwest's URL
    // or headers. Upstream response bodies are scrubbed separately below.
    let kind = match failure {
        AdapterFailure::ConnectError => "connection failed",
        AdapterFailure::RemoteProtocolError => "remote protocol error",
        AdapterFailure::PoolTimeout => "connection pool timeout",
        AdapterFailure::ReadTimeout => "read timeout",
        AdapterFailure::WriteTimeout => "write timeout",
        AdapterFailure::ReadError => "response read failed",
        AdapterFailure::WriteError => "request write failed",
        AdapterFailure::OperationTimeout => "request timeout",
        AdapterFailure::HttpStatus(_) | AdapterFailure::Other => "upstream request failed",
    };
    let endpoint = display_endpoint(endpoint);
    validation_detail(format!(
        "LangGraph {operation} request error at {endpoint}: {kind}"
    ))
}
fn embedded_error(result: &Value) -> Option<String> {
    let error = result.get("__error__")?;
    let text = if error.is_object() {
        let kind = text_or(error, &["error", "type"]);
        let kind = if kind.is_empty() {
            "LangGraphError"
        } else {
            &kind
        };
        format!("{kind}: {}", text_or(error, &["message", "detail"]))
    } else {
        pystr(error)
    };
    Some(scrub_detail(&text)).filter(|text| !text.is_empty())
}
fn decode_utf8_ignore(mut bytes: &[u8]) -> String {
    let mut output = String::new();
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(text) => {
                output.push_str(text);
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                output.push_str(std::str::from_utf8(&bytes[..valid]).expect("valid prefix"));
                if let Some(length) = error.error_len() {
                    bytes = &bytes[valid + length..];
                } else {
                    break;
                }
            }
        }
    }
    output
}
impl ProtocolAdapter for LangGraphAdapter {
    fn invoke<'a>(&'a self, request: &'a UnifiedRequest) -> AdapterFuture<'a, UnifiedResponse> {
        Box::pin(async move {
            let (endpoint, payload) = self.payload(request, false).await?;
            let endpoint_label = display_endpoint(&endpoint);
            let reply = self
                .http
                .post_json(&endpoint, &payload, self.headers(request)?)
                .await
                .map_err(|error| request_error("invoke", &endpoint, error))?;
            let status = reply.response.status().as_u16();
            let is_json = reply
                .response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.contains("application/json"));
            let bytes = reply.response.bytes().await.map_err(|error| {
                request_error(
                    "invoke",
                    &endpoint,
                    super::transport::transport_error(&error, true),
                )
            })?;
            if !(200..300).contains(&status) {
                return Err(validation_detail(format!(
                    "LangGraph invoke failed ({status}) at {endpoint_label}: {}",
                    scrub_detail(&String::from_utf8_lossy(&bytes))
                )));
            }
            let result = if is_json {
                serde_json::from_slice::<Value>(&bytes)
                    .map_err(|_| validation("LangGraph response JSON invalid"))?
            } else {
                json!(String::from_utf8_lossy(&bytes))
            };
            if let Some(error) = embedded_error(&result) {
                return Err(validation_detail(format!(
                    "LangGraph invoke failed at {endpoint_label}: {error}"
                )));
            }
            let mut response = UnifiedResponse::new(
                &request.request_id,
                "success",
                vec![content(ContentType::Text, json!(result_text(&result)))],
            );
            response.session_id = request.session_id.clone();
            Ok(response)
        })
    }
    fn stream(
        &self,
        request: UnifiedRequest,
    ) -> AdapterFuture<'_, Box<dyn DispatchProducer<StreamChunk>>> {
        Box::pin(async move {
            let (endpoint, payload) = self.payload(&request, true).await?;
            let endpoint_label = display_endpoint(&endpoint);
            let response = self
                .stream_http
                .post_json(&endpoint, &payload, self.headers(&request)?)
                .await
                .map_err(|error| request_error("stream", &endpoint, error))?;
            let status = response.response.status().as_u16();
            if status != 200 {
                let bytes = response.response.bytes().await.map_err(|error| {
                    request_error(
                        "stream",
                        &endpoint,
                        super::transport::transport_error(&error, true),
                    )
                })?;
                return Err(validation_detail(format!(
                    "LangGraph stream failed ({status}) at {endpoint_label}: {}",
                    scrub_detail(&decode_utf8_ignore(&bytes))
                )));
            }
            Ok(Box::new(LangGraphStream {
                lines: LangGraphLines::new(response, endpoint),
                request_id: request.request_id,
                index: 0,
                event: String::new(),
                usage: None,
                thinking: false,
                finished: false,
                pending: VecDeque::new(),
                projection: Projection::default(),
            }) as Box<dyn DispatchProducer<StreamChunk>>)
        })
    }
    fn health_check(&self) -> AdapterFuture<'_, bool> {
        Box::pin(async {
            let mut request =
                UnifiedRequest::new("health-check", self.service.service_id(), vec![]);
            request.user_id = "gateway-health-check".into();
            request.tenant_id = "gateway".into();
            let config = self.config.snapshot();
            let headers = if config.get("health_skip_auth").is_some_and(python_truthy) {
                HeaderMap::new()
            } else {
                match self.headers(&request) {
                    Ok(h) => h,
                    Err(_) => return Ok(false),
                }
            };
            let endpoint = config
                .get("health_endpoint")
                .and_then(Value::as_str)
                .unwrap_or("/health");
            let Ok(url) = self.http.url(endpoint) else {
                return Ok(false);
            };
            let Ok(response) = self
                .http
                .send(self.http.client.get(url).headers(headers))
                .await
            else {
                return Ok(false);
            };
            Ok(
                response.response.status().as_u16() < 500
                    && response.response.bytes().await.is_ok(),
            )
        })
    }
}
fn result_text(result: &Value) -> String {
    let messages = result
        .get("messages")
        .filter(|v| python_truthy(v))
        .or_else(|| {
            result
                .pointer("/values/messages")
                .filter(|v| python_truthy(v))
        })
        .or_else(|| result.pointer("/output/messages"))
        .and_then(Value::as_array);
    if let Some(messages) = messages.filter(|v| !v.is_empty()) {
        let role = |msg: &Value| {
            msg.get("role")
                .filter(|v| python_truthy(v))
                .or_else(|| msg.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        if let Some(msg) = messages
            .iter()
            .rev()
            .find(|msg| matches!(role(msg).as_str(), "assistant" | "ai" | "AIMessage"))
        {
            return msg
                .get("content")
                .filter(|v| python_truthy(v))
                .map(pystr)
                .unwrap_or_default();
        }
        let last = messages.last().expect("nonempty");
        if last.is_object() && !matches!(role(last).as_str(), "user" | "human" | "HumanMessage") {
            return last
                .get("content")
                .filter(|v| python_truthy(v))
                .map(pystr)
                .unwrap_or_default();
        }
    }
    pystr(result)
}

#[derive(Clone, Debug)]
pub struct StreamEvent {
    pub event_type: StreamEventType,
    pub text: Option<String>,
    pub tool_call: Option<ToolCall>,
}
impl StreamEvent {
    fn text(value: String) -> Self {
        Self {
            event_type: StreamEventType::TextDelta,
            text: Some(value),
            tool_call: None,
        }
    }
    fn tool(
        kind: StreamEventType,
        id: String,
        name: String,
        arguments: String,
        result: Option<String>,
    ) -> Self {
        let status = if kind == StreamEventType::ToolResult {
            "completed"
        } else {
            "running"
        };
        Self {
            event_type: kind,
            text: result,
            tool_call: Some(ToolCall {
                tool_call_id: id,
                name,
                arguments,
                status: status.into(),
            }),
        }
    }
    fn with_tool_result_status(mut self, message: &Value) -> Self {
        let status = message.get("status").and_then(Value::as_str).unwrap_or("");
        if let Some(call) = self.tool_call.as_mut() {
            call.status = if matches!(status.to_ascii_lowercase().as_str(), "error" | "failed") {
                "error".into()
            } else {
                "completed".into()
            };
        }
        self
    }
    fn into_chunk(self, request: &str, index: i64) -> StreamChunk {
        let text = self.text.unwrap_or_default();
        StreamChunk {
            request_id: request.into(),
            chunk_index: index,
            content: content(
                if text.is_empty() {
                    ContentType::ToolCall
                } else {
                    ContentType::Text
                },
                json!(text),
            ),
            is_final: false,
            event_type: self.event_type,
            tool_call: self.tool_call,
            metadata: None,
        }
    }
}
fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}
fn text_or(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| value.get(*key).filter(|v| python_truthy(v)))
        .map(pystr)
        .unwrap_or_default()
}
fn json_arguments(value: &Value, ascii: bool) -> String {
    if value.is_object() {
        crate::python_json(value, ascii, true)
    } else if python_truthy(value) {
        pystr(value)
    } else {
        String::new()
    }
}
fn args_value(value: &Value, keys: &[&str], ascii: bool) -> String {
    keys.iter()
        .find_map(|key| value.get(*key).filter(|v| python_truthy(v)))
        .map(|v| json_arguments(v, ascii))
        .unwrap_or_default()
}
fn placeholder(value: &str) -> bool {
    serde_json::from_str::<Value>(value)
        .ok()
        .and_then(|v| {
            v.as_object().map(|v| {
                v.values().all(|v| {
                    v.is_null() || v.as_str() == Some("") || v.as_array().is_some_and(Vec::is_empty)
                })
            })
        })
        .unwrap_or(false)
}
fn has_arguments(value: &str) -> bool {
    match serde_json::from_str::<Value>(value) {
        Ok(Value::Object(map)) => map.values().any(python_truthy),
        _ => !value.is_empty(),
    }
}

/// Every remote event can yield tool and text updates together; no text is lost
/// when a message's first tool-call member takes the primary branch.
#[derive(Default)]
pub struct Projection {
    pub last_content_length: usize,
    pub last_tool_args: HashMap<String, String>,
    pub current_tool_call_id: String,
    pub current_tool_name: String,
    pub sent_tool_call_starts: HashSet<String>,
}
impl Projection {
    fn key(&self) -> &str {
        if self.current_tool_call_id.is_empty() {
            "default"
        } else {
            &self.current_tool_call_id
        }
    }
    fn tool_delta(&mut self, args: String) -> StreamEvent {
        let key = self.key().to_owned();
        let event = if self.sent_tool_call_starts.insert(key) {
            StreamEventType::ToolCallStart
        } else {
            StreamEventType::ToolCallDelta
        };
        StreamEvent::tool(
            event,
            self.current_tool_call_id.clone(),
            self.current_tool_name.clone(),
            args,
            None,
        )
    }
    fn track(&mut self, call: &Value) {
        let id = text_or(call, &["id", "tool_call_id"]);
        let name = text_or(call, &["name"]);
        if !id.is_empty() {
            self.current_tool_call_id = id;
        }
        if !name.is_empty() {
            self.current_tool_name = name;
        }
    }
    pub fn ai_text_delta(&mut self, msg: &Value) -> Option<StreamEvent> {
        if !msg.is_object()
            || !matches!(
                msg.get("type").and_then(Value::as_str),
                Some("ai" | "AIMessage" | "AIMessageChunk")
            )
        {
            return None;
        }
        let value = msg.get("content").unwrap_or(&Value::Null);
        let content = if let Some(items) = value.as_array() {
            items
                .iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Object(_) if v["type"] == "text" => {
                        Some(v.get("text").map(pystr).unwrap_or_default())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        } else {
            value.as_str()?.into()
        };
        let length = content.chars().count();
        if length <= self.last_content_length {
            return None;
        }
        let delta = content.chars().skip(self.last_content_length).collect();
        self.last_content_length = length;
        Some(StreamEvent::text(delta))
    }
    pub fn observe(&mut self, event: &Value) -> Vec<StreamEvent> {
        let primary = self.primary(event);
        let mut events = primary.into_iter().collect::<Vec<_>>();
        if !events
            .iter()
            .any(|v| v.event_type == StreamEventType::TextDelta)
        {
            let kind = event.get("event").and_then(Value::as_str).unwrap_or("");
            if matches!(kind, "messages" | "messages/partial") {
                let data = if event.get("event").is_some() {
                    &event["data"]
                } else {
                    event
                };
                let message = data.as_array().and_then(|v| v.first()).unwrap_or(data);
                if let Some(text) = self.ai_text_delta(message) {
                    events.push(text);
                }
            }
        }
        events
    }
    fn primary(&mut self, event: &Value) -> Option<StreamEvent> {
        if !event.is_object() {
            return None;
        }
        let kind = event.get("event").and_then(Value::as_str).unwrap_or("");
        let data = if event.get("event").is_some() {
            &event["data"]
        } else {
            event
        };
        if kind == "messages/complete" {
            self.last_content_length = 0;
            self.last_tool_args.clear();
            self.current_tool_call_id.clear();
            self.current_tool_name.clear();
            return None;
        }
        if matches!(kind, "end" | "error" | "metadata") {
            return None;
        }
        if matches!(kind, "messages" | "messages/partial") {
            let msg = data.as_array().and_then(|v| v.first()).unwrap_or(data);
            if !msg.is_object() {
                return None;
            }
            if let Some(call) = msg["tool_call_chunks"].as_array().and_then(|v| v.first()) {
                self.track(call);
                let args = args_value(call, &["args"], false);
                let name = text_or(call, &["name"]);
                let trimmed = args.trim();
                if trimmed.starts_with('{') && trimmed.ends_with('}') {
                    let previous = self
                        .last_tool_args
                        .get(self.key())
                        .map(String::as_str)
                        .unwrap_or("");
                    if args.chars().count() <= previous.chars().count() || placeholder(&args) {
                        return None;
                    }
                    self.last_tool_args.insert(self.key().into(), args.clone());
                }
                if !name.is_empty() || !args.is_empty() {
                    return Some(self.tool_delta(args));
                }
                return None;
            }
            if let Some(call) = msg["tool_calls"].as_array().and_then(|v| v.first()) {
                self.track(call);
                let args = args_value(call, &["args", "arguments"], false);
                let previous = self
                    .last_tool_args
                    .get(self.key())
                    .map(String::as_str)
                    .unwrap_or("");
                if placeholder(previous) || args.chars().count() > previous.chars().count() {
                    self.last_tool_args.insert(self.key().into(), args.clone());
                    return Some(self.tool_delta(args));
                }
                return None;
            }
            if matches!(
                msg.get("type").and_then(Value::as_str),
                Some("tool" | "ToolMessage")
            ) {
                let id = string(msg, "tool_call_id");
                let name = string(msg, "name");
                let body = msg.get("content").unwrap_or(&Value::Null);
                let content = if body.is_object() {
                    crate::python_json(body, false, true)
                } else if msg.get("content").is_none() {
                    String::new()
                } else {
                    pystr(body)
                };
                let mut args = String::new();
                for field in ["artifact", "additional_kwargs"] {
                    if msg[field].is_object() && msg[field].get("query").is_some() {
                        args = crate::python_json(
                            &json!({"query":msg[field]["query"]}),
                            false,
                            true,
                        );
                        break;
                    }
                }
                if args.is_empty() && msg["tool_input"].is_object() {
                    let value = if msg["tool_input"].get("query").is_some() {
                        json!({"query":msg["tool_input"]["query"]})
                    } else {
                        msg["tool_input"].clone()
                    };
                    args = crate::python_json(&value, false, true);
                }
                if args.is_empty() {
                    let stored = self
                        .last_tool_args
                        .get(&id)
                        .filter(|v| !v.is_empty())
                        .or_else(|| self.last_tool_args.get(&self.current_tool_call_id));
                    if let Some(stored) = stored.filter(|v| has_arguments(v)) {
                        args = stored.clone();
                    }
                }
                self.current_tool_call_id.clear();
                self.current_tool_name.clear();
                return Some(StreamEvent::tool(
                    StreamEventType::ToolResult,
                    id,
                    name,
                    args,
                    Some(content),
                ).with_tool_result_status(msg));
            }
            return self.ai_text_delta(msg);
        }
        if kind == "updates" {
            if let Some(messages) = data["tools"]["messages"].as_array() {
                for msg in messages {
                    if msg.is_object()
                        && matches!(
                            msg.get("type").and_then(Value::as_str),
                            Some("tool" | "ToolMessage")
                        )
                    {
                        let id = string(msg, "tool_call_id");
                        let name = string(msg, "name");
                        let body = msg.get("content").unwrap_or(&Value::Null);
                        let content = if body.is_object() {
                            crate::python_json(body, false, true)
                        } else if msg.get("content").is_none() {
                            String::new()
                        } else {
                            pystr(body)
                        };
                        let args = self
                            .last_tool_args
                            .get(&id)
                            .filter(|v| !v.is_empty())
                            .or_else(|| self.last_tool_args.get(&self.current_tool_call_id))
                            .cloned()
                            .unwrap_or_default();
                        return Some(StreamEvent::tool(
                            StreamEventType::ToolResult,
                            id,
                            name,
                            args,
                            Some(content),
                        ).with_tool_result_status(msg));
                    }
                }
            }
        }
        None
    }
}

/// Source's JSON-compatible local astream_events projection. An in-process
/// Python graph still needs an explicit native implementation registered by host.
pub fn extract_local_event(event: &Value, seen: &mut HashSet<String>) -> Option<StreamEvent> {
    if !event.is_object() {
        return None;
    }
    let kind = event.get("event").and_then(Value::as_str).unwrap_or("");
    let run = string(event, "run_id");
    let data = &event["data"];
    match kind {
        "on_chat_model_stream" => {
            let value = data.get("chunk")?;
            if value.is_null() {
                return None;
            }
            let id = text_or(value, &["id"]);
            if !id.is_empty() && !seen.insert(id) {
                return None;
            }
            let calls = value
                .get("tool_call_chunks")
                .filter(|v| python_truthy(v))
                .or_else(|| value.get("tool_calls"));
            if let Some(call) = calls.and_then(Value::as_array).and_then(|v| v.first()) {
                let name = text_or(call, &["name"]);
                let args = args_value(call, &["args", "arguments"], true);
                if !name.is_empty() || !args.is_empty() {
                    return Some(StreamEvent::tool(
                        StreamEventType::ToolCallDelta,
                        text_or(call, &["id", "tool_call_id"]),
                        name,
                        args,
                        None,
                    ));
                }
            }
            value["content"]
                .as_str()
                .filter(|v| !v.is_empty())
                .map(|v| StreamEvent::text(v.into()))
        }
        "on_tool_start" => {
            if !seen.insert(format!("tool_start:{run}")) {
                return None;
            }
            let input = data.get("input").cloned().unwrap_or(json!({}));
            let args = if input.is_object() {
                crate::python_json(&input, false, true)
            } else {
                pystr(&input)
            };
            Some(StreamEvent::tool(
                StreamEventType::ToolCallStart,
                run,
                string(event, "name"),
                args,
                None,
            ))
        }
        "on_tool_end" => {
            if !seen.insert(format!("tool_end:{run}")) {
                return None;
            }
            let output = data.get("output").cloned().unwrap_or(json!(""));
            let output_message = output.clone();
            let output = if output.is_object() {
                crate::python_json(&output, false, true)
            } else {
                pystr(&output)
            };
            Some(StreamEvent::tool(
                StreamEventType::ToolResult,
                run,
                string(event, "name"),
                String::new(),
                Some(output),
            ).with_tool_result_status(&output_message))
        }
        _ => None,
    }
}

/// Preserve LangGraph's per-HTTP-chunk UTF-8 ignore and newline-only SSE parser.
/// Source discards an incomplete last line; it does not merge multiline data.
struct LangGraphLines {
    response: Option<HttpResponse>,
    buffer: String,
    endpoint: String,
}
impl LangGraphLines {
    fn new(response: HttpResponse, endpoint: String) -> Self {
        Self {
            response: Some(response),
            buffer: String::new(),
            endpoint,
        }
    }
    fn close(&mut self) {
        self.response = None;
        self.buffer.clear();
    }
    async fn next(&mut self) -> Result<Option<String>, AdapterError> {
        loop {
            if let Some(end) = self.buffer.find('\n') {
                let line = self.buffer[..end].to_owned();
                self.buffer = self.buffer[end + 1..].into();
                return Ok(Some(line));
            }
            let Some(response) = &mut self.response else {
                return Ok(None);
            };
            match response.response.chunk().await.map_err(|error| {
                request_error(
                    "stream",
                    &self.endpoint,
                    super::transport::transport_error(&error, true),
                )
            })? {
                Some(chunk) => {
                    self.buffer.push_str(&decode_utf8_ignore(&chunk));
                }
                None => {
                    self.close();
                    return Ok(None);
                }
            }
        }
    }
}
struct LangGraphStream {
    lines: LangGraphLines,
    request_id: String,
    index: i64,
    event: String,
    usage: Option<Map<String, Value>>,
    thinking: bool,
    finished: bool,
    pending: VecDeque<StreamEvent>,
    projection: Projection,
}
impl LangGraphStream {
    async fn read(&mut self) -> Result<Option<StreamChunk>, AdapterError> {
        if self.finished {
            return Ok(None);
        }
        loop {
            if let Some(event) = self.pending.pop_front() {
                let chunk = event.into_chunk(&self.request_id, self.index);
                self.index += 1;
                return Ok(Some(chunk));
            }
            let Some(line) = self.lines.next().await? else {
                return Ok(Some(self.final_chunk()));
            };
            let line = line.trim();
            if line.is_empty() {
                self.event.clear();
                continue;
            }
            if let Some(event) = line.strip_prefix("event:") {
                self.event = event.trim().into();
                // Error events, including documented subgraph namespaces, are
                // terminal even when the HTTP response itself succeeded. Do not
                // read or expose a provider's potentially sensitive error body.
                if self.event.split('|').next() == Some("error") {
                    return Err(validation("langgraph_provider_error"));
                }
                if self.event == "end" {
                    return Ok(Some(self.final_chunk()));
                }
                continue;
            }
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() || data == "[DONE]" {
                return Ok(Some(self.final_chunk()));
            }
            let Ok(data) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            if self.event.is_empty()
                && data.get("event").and_then(Value::as_str)
                    .is_some_and(|event| event.split('|').next() == Some("error"))
            {
                return Err(validation("langgraph_provider_error"));
            }
            if self.event == "metadata" {
                if let Some(usage) = data["usage"].as_object() {
                    self.usage = Some(usage.clone());
                }
                continue;
            }
            let event = if self.event.is_empty() {
                data
            } else {
                json!({"event":self.event,"data":data})
            };
            if !self.thinking && !self.event.is_empty() {
                self.thinking = true;
                self.pending.push_back(StreamEvent {
                    event_type: StreamEventType::Thinking,
                    text: None,
                    tool_call: None,
                });
            }
            self.pending.extend(self.projection.observe(&event));
        }
    }
    fn final_chunk(&mut self) -> StreamChunk {
        self.lines.close();
        self.finished = true;
        let mut final_chunk = chunk(
            &self.request_id,
            self.index,
            json!(""),
            StreamEventType::Final,
            true,
        );
        if let Some(usage) = self.usage.take().filter(|v| !v.is_empty()) {
            final_chunk.metadata = Some(Map::from_iter([("usage".into(), json!(usage))]));
        }
        final_chunk
    }
}
impl DispatchProducer<StreamChunk> for LangGraphStream {
    fn next(&mut self) -> DispatchFuture<'_, Option<StreamChunk>> {
        Box::pin(async move {
            match self.read().await {
                Ok(chunk) => Ok(chunk),
                Err(error) => {
                    self.lines.close();
                    self.finished = true;
                    self.pending.clear();
                    self.usage = None;
                    Err(dispatch(error))
                }
            }
        })
    }
    fn close(mut self: Box<Self>) -> DispatchFuture<'static, ()> {
        Box::pin(async move {
            self.lines.close();
            self.pending.clear();
            self.finished = true;
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "langgraph_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "langgraph_thread_tests.rs"]
mod thread_tests;
