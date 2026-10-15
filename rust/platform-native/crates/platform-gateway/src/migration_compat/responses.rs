//! OpenAI Responses validation, deterministic event projection and native idempotency.
//! The host supplies events from its canonical Agent Runtime control-plane adapter.
use super::common::{CompatError, CompatUser, hex, object, text};
use redis::{AsyncCommands, aio::ConnectionManager};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

fn invalid(message: &str) -> CompatError {
    CompatError::new(400, message)
}
fn unsupported(code: &str) -> CompatError {
    CompatError::value(409, json!({"code":code}))
}
fn tool_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.len() <= 128
        && chars.all(|c| c.is_ascii_alphanumeric() || "_.:-".contains(c))
}
pub fn responses_tool_config(payload: &Value) -> Result<Value, CompatError> {
    let mut names = Vec::<String>::new();
    let mut tools = Vec::new();
    if let Some(raw) = payload.get("tools").filter(|v| !v.is_null()) {
        let raw = raw
            .as_array()
            .filter(|v| v.len() <= 128)
            .ok_or_else(|| invalid("tools must be an array of at most 128 items"))?;
        for tool in raw {
            if !tool.is_object() || text(tool, "type") != "function" {
                return Err(unsupported("responses_tool_type_not_migrated"));
            }
            let function = tool
                .get("function")
                .filter(|v| v.is_object())
                .unwrap_or(tool);
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .filter(|v| tool_name(v))
                .ok_or_else(|| invalid("function tool name is invalid"))?;
            let description = function.get("description").unwrap_or(&Value::Null);
            let description = if description.is_null() && function.get("description").is_none() {
                ""
            } else {
                description
                    .as_str()
                    .filter(|v| v.chars().count() <= 20_000)
                    .ok_or_else(|| invalid("function tool description is invalid"))?
            };
            let parameters = function
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({"type":"object","properties":{}}));
            if !parameters.is_object() {
                return Err(invalid("function tool parameters must be an object"));
            }
            if names.iter().any(|v| v == name) {
                return Err(invalid("function tool names must be unique"));
            }
            names.push(name.to_owned());
            let mut public = json!({"type":"function","name":name,"description":description,"parameters":parameters});
            if function["strict"].is_boolean() {
                public["strict"] = function["strict"].clone();
            }
            tools.push(public);
        }
    }
    let mut choice = payload
        .get("tool_choice")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(json!("auto"));
    if !matches!(choice.as_str(), Some("auto" | "none" | "required")) {
        if text(&choice, "type") != "function" {
            return Err(invalid("tool_choice is invalid"));
        }
        let name = if choice["function"].is_object() {
            text(&choice["function"], "name")
        } else {
            text(&choice, "name")
        };
        if !tool_name(name) {
            return Err(invalid("tool_choice function name is invalid"));
        }
        choice = json!({"type":"function","name":name});
    }
    if choice.is_object() && !names.iter().any(|v| v == text(&choice, "name")) {
        return Err(invalid("tool_choice references an unavailable tool"));
    }
    if choice == json!("required") && names.is_empty() {
        return Err(invalid("required tool_choice needs at least one tool"));
    }
    let parallel = payload
        .get("parallel_tool_calls")
        .map(|v| {
            v.as_bool()
                .ok_or_else(|| invalid("parallel_tool_calls must be a boolean"))
        })
        .transpose()?
        .unwrap_or(true);
    Ok(
        json!({"tool_names":names,"public_tools":tools,"tool_choice":choice,"parallel_tool_calls":parallel}),
    )
}
pub fn responses_message(payload: &Value) -> Result<String, CompatError> {
    let value = &payload["input"];
    if let Some(value) = value.as_str().filter(|v| !v.trim().is_empty()) {
        return Ok(value.to_owned());
    }
    let list = value
        .as_array()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| invalid("input must be a non-empty string or message list"))?;
    let mut result = Vec::new();
    for item in list {
        if !item.is_object() {
            return Err(invalid("input items must be objects"));
        }
        let kind = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message");
        if !matches!(kind, "message" | "input_text") {
            return Err(CompatError::value(
                409,
                json!({"code":"responses_input_item_not_migrated","item_type":kind}),
            ));
        }
        let empty = json!("");
        let content = item
            .get("content")
            .or_else(|| item.get("text"))
            .unwrap_or(&empty);
        match content {
            Value::String(value) => result.push(value.clone()),
            Value::Array(parts) => {
                for part in parts {
                    if !part.is_object() || !matches!(text(part, "type"), "input_text" | "text") {
                        return Err(unsupported("responses_input_item_not_migrated"));
                    }
                    result.push(
                        part["text"]
                            .as_str()
                            .ok_or_else(|| invalid("input text must be a string"))?
                            .into(),
                    );
                }
            }
            _ => return Err(invalid("input content is invalid")),
        }
    }
    let result = result
        .into_iter()
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if result.is_empty() {
        Err(invalid("input must contain text"))
    } else {
        Ok(result)
    }
}
pub fn reject_unmigrated_fields(payload: &Value) -> Result<(), CompatError> {
    let mut fields = Vec::new();
    for field in [
        "attachments",
        "conversation",
        "previous_response_id",
        "store",
    ] {
        if let Some(value) = payload.get(field) {
            let absent = if field == "store" {
                value.is_null() || value == &json!(false) || value == &json!(0)
            } else {
                value.is_null() || value == &json!([]) || value == &json!("")
            };
            if !absent {
                fields.push(field);
            }
        }
    }
    if fields.is_empty() {
        Ok(())
    } else {
        Err(CompatError::value(
            409,
            json!({"code":"responses_fields_not_migrated","fields":fields}),
        ))
    }
}
#[derive(Clone)]
pub struct ResponsesRequest {
    pub model: String,
    pub provider_id: Option<String>,
    pub session_id: String,
    pub message: String,
    pub tools: Value,
    pub payload: Value,
    pub stream: bool,
}
impl ResponsesRequest {
    /// Call after tenant authentication, rate limits, runtime-field rejection and model authorization.
    pub fn after_model_authorization(
        payload: Value,
        idempotency_key: &str,
    ) -> Result<Self, CompatError> {
        reject_unmigrated_fields(&payload)?;
        let tools = responses_tool_config(&payload)?;
        let message = responses_message(&payload)?;
        let model = payload["model"]
            .as_str()
            .filter(|v| !v.trim().is_empty() && v.chars().count() <= 255)
            .ok_or_else(|| invalid("model must be a non-empty string."))?
            .trim()
            .to_owned();
        if let Some(value) = payload.get("instructions").filter(|v| !v.is_null()) {
            if value
                .as_str()
                .is_none_or(|v| v.trim().is_empty() || v.chars().count() > 256 * 1024)
            {
                return Err(invalid("instructions must be a non-empty string"));
            }
        }
        if let Some(reasoning) = payload.get("reasoning").filter(|v| !v.is_null()) {
            if !reasoning.is_object() {
                return Err(invalid("reasoning must be an object"));
            }
            if reasoning
                .get("effort")
                .is_some_and(|v| !v.is_null() && !v.is_string())
            {
                return Err(invalid("reasoning.effort must be a string"));
            }
        }
        if payload
            .get("max_output_tokens")
            .is_some_and(|v| !v.is_null() && v.as_u64().is_none_or(|v| v < 1))
        {
            return Err(invalid("max_output_tokens must be a positive integer"));
        }
        if payload
            .get("temperature")
            .is_some_and(|v| !v.is_null() && v.as_f64().is_none_or(|v| !(0.0..=2.0).contains(&v)))
        {
            return Err(invalid("temperature must be between 0 and 2"));
        }
        let stream = payload
            .get("stream")
            .map(|v| {
                v.as_bool()
                    .ok_or_else(|| invalid("stream must be a boolean"))
            })
            .transpose()?
            .unwrap_or(false);
        if stream && !idempotency_key.trim().is_empty() {
            return Err(unsupported("streaming_idempotency_not_supported"));
        }
        let session_id = payload
            .get("session_id")
            .filter(|v| crate::service_registry::python_truthy(v))
            .map(crate::usage_parser::python_value_to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let provider_id = payload
            .get("provider_id")
            .and_then(Value::as_str)
            .map(|v| v.trim().to_owned());
        Ok(Self {
            model,
            provider_id,
            session_id,
            message,
            tools,
            payload,
            stream,
        })
    }
    pub fn readonly_capabilities(&self) -> Value {
        let names = self.tools["tool_names"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut result = json!({"responses_tool_choice":if names.is_empty(){json!("none")}else{self.tools["tool_choice"].clone()},"responses_parallel_tool_calls":self.tools["parallel_tool_calls"]});
        if !names.is_empty() {
            result["responses_tool_names"] = json!(names);
        }
        result
    }
}
pub fn responses_usage(value: Option<&Value>, input: &Value, output: &str) -> Value {
    let empty = json!({});
    let value = value.filter(|v| v.is_object()).unwrap_or(&empty);
    let token = |a: &str, b: Option<&str>| {
        value
            .get(a)
            .and_then(Value::as_u64)
            .or_else(|| b.and_then(|b| value.get(b)).and_then(Value::as_u64))
            .unwrap_or(0)
    };
    let estimate = |value: &Value| ((value.to_string().as_bytes().len() + 3) / 4).max(1) as u64;
    let mut input_tokens = token("input_tokens", Some("prompt_tokens"));
    let mut output_tokens = token("output_tokens", Some("completion_tokens"));
    if input_tokens == 0 && !input.is_null() && input != &json!("") {
        input_tokens = estimate(input);
    }
    if output_tokens == 0 && !output.is_empty() {
        output_tokens = estimate(&json!(output));
    }
    let cached = value["input_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or_else(|| token("cached_input_tokens", None))
        .min(input_tokens);
    json!({"input_tokens":input_tokens,"input_tokens_details":{"cached_tokens":cached},"output_tokens":output_tokens,"output_tokens_details":{"reasoning_tokens":token("reasoning_tokens",None)},"total_tokens":input_tokens+output_tokens})
}
pub fn response_function_call(data: &Value) -> Option<Value> {
    let call_id = data
        .get("tool_call_id")
        .filter(|v| crate::service_registry::python_truthy(v))
        .or_else(|| data.get("call_id"))
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())?;
    let name = data
        .get("tool_name")
        .filter(|v| crate::service_registry::python_truthy(v))
        .or_else(|| data.get("name"))
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())?;
    let arguments = data
        .get("arguments")
        .map(|v| {
            if v.is_object() || v.is_array() {
                v.to_string()
            } else {
                crate::usage_parser::python_value_to_string(v)
            }
        })
        .unwrap_or_default();
    Some(
        json!({"id":call_id,"type":"function_call","status":if matches!(text(data,"status"),"completed"|"succeeded"){"completed"}else{"in_progress"},"call_id":call_id,"name":name,"arguments":arguments}),
    )
}
#[derive(Clone)]
pub struct ResponseProjection {
    pub response_id: String,
    pub model: String,
    pub input: Value,
    pub tools: Value,
    pub text: String,
    calls: Vec<Value>,
    pub usage: Option<Value>,
    pub terminal: Option<String>,
    sequence: u64,
}
impl ResponseProjection {
    pub fn new(run_id: &str, request: &ResponsesRequest) -> Self {
        Self {
            response_id: format!("resp_{}", run_id.replace('-', "")),
            model: request.model.clone(),
            input: json!(request.message),
            tools: request.tools.clone(),
            text: String::new(),
            calls: vec![],
            usage: None,
            terminal: None,
            sequence: 0,
        }
    }
    fn item_id(&self) -> String {
        format!(
            "msg_{}",
            self.response_id
                .strip_prefix("resp_")
                .unwrap_or(&self.response_id)
        )
    }
    fn emit(&mut self, event: &str, payload: Value) -> Vec<u8> {
        let mut data = object(&payload);
        data.insert("type".into(), json!(event));
        data.insert("sequence_number".into(), json!(self.sequence));
        self.sequence += 1;
        format!("event: {event}\ndata: {}\n\n", Value::Object(data)).into_bytes()
    }
    pub fn initial_events(&mut self) -> Vec<Vec<u8>> {
        let progress = json!({"id":self.response_id,"object":"response","created_at":chrono::Utc::now().timestamp(),"status":"in_progress","model":self.model,"output":[],"parallel_tool_calls":self.tools["parallel_tool_calls"],"tool_choice":self.tools["tool_choice"],"tools":self.tools["public_tools"],"usage":null});
        vec![self.emit("response.created",json!({"response":progress})),self.emit("response.in_progress",json!({"response":progress})),self.emit("response.output_item.added",json!({"output_index":0,"item":{"id":self.item_id(),"type":"message","role":"assistant","status":"in_progress","content":[]}})),self.emit("response.content_part.added",json!({"item_id":self.item_id(),"output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}))]
    }
    pub fn completed_response(&self, error: Option<Value>) -> Value {
        let failed = error.is_some();
        let mut output = self.calls.clone();
        output.push(json!({"id":self.item_id(),"type":"message","role":"assistant","status":if failed{"incomplete"}else{"completed"},"content":[{"type":"output_text","text":self.text,"annotations":[]}]}));
        json!({"id":self.response_id,"object":"response","created_at":chrono::Utc::now().timestamp(),"status":if failed{"failed"}else{"completed"},"error":error,"incomplete_details":null,"model":self.model,"output":output,"output_text":self.text,"parallel_tool_calls":self.tools["parallel_tool_calls"],"tool_choice":self.tools["tool_choice"],"tools":self.tools["public_tools"],"usage":responses_usage(self.usage.as_ref(),&self.input,&self.text)})
    }
    pub fn observe(&mut self, event: &Value) -> Vec<Vec<u8>> {
        if self.terminal.is_some() {
            return vec![];
        }
        let data = event
            .get("data")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or(json!({}));
        let kind = text(event, "event_type");
        let mut out = vec![];
        match kind {
            "text_delta" => {
                if let Some(content) = data["content"].as_str() {
                    self.text.push_str(content);
                    out.push(self.emit("response.output_text.delta",json!({"item_id":self.item_id(),"output_index":0,"content_index":0,"delta":content,"logprobs":[]})));
                }
            }
            "thinking_delta" => {
                if let Some(content) = data["content"].as_str() {
                    out.push(self.emit("response.reasoning_summary_text.delta",json!({"item_id":data.get("item_id").filter(|v|crate::service_registry::python_truthy(v)).cloned().unwrap_or(json!(format!("reasoning_{}",self.response_id.trim_start_matches("resp_")))),"output_index":0,"summary_index":data["summary_index"].as_i64().unwrap_or(0),"delta":content})));
                }
            }
            "tool_call_start" => {
                if let Some(call) = response_function_call(&data) {
                    if let Some(index) = self
                        .calls
                        .iter()
                        .position(|v| v["call_id"] == call["call_id"])
                    {
                        self.calls[index] = call.clone();
                    } else {
                        self.calls.push(call.clone());
                    }
                    let index = self.calls.len();
                    out.push(self.emit(
                        "response.output_item.added",
                        json!({"output_index":index,"item":call}),
                    ));
                    if !text(&call, "arguments").is_empty() {
                        out.push(self.emit("response.function_call_arguments.delta",json!({"item_id":call["call_id"],"output_index":index,"delta":call["arguments"]})));
                    }
                    out.push(self.emit("response.function_call_arguments.done",json!({"item_id":call["call_id"],"output_index":index,"name":call["name"],"arguments":call["arguments"]})));
                }
            }
            "tool_call_result" | "tool_call_end" => {
                let id = data
                    .get("tool_call_id")
                    .filter(|v| crate::service_registry::python_truthy(v))
                    .or_else(|| data.get("call_id"));
                if let Some(index) = self.calls.iter().position(|v| Some(&v["call_id"]) == id) {
                    if kind == "tool_call_result" {
                        self.calls[index]["status"] = json!(if matches!(
                            text(&data, "status"),
                            "completed" | "succeeded"
                        ) {
                            "completed"
                        } else {
                            "failed"
                        });
                    } else {
                        out.push(self.emit(
                            "response.output_item.done",
                            json!({"output_index":index+1,"item":self.calls[index]}),
                        ));
                    }
                }
            }
            "run_finished" | "run_error" => {
                let failed = kind == "run_error";
                let status = if failed {
                    data.get("status")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .unwrap_or("failed")
                } else {
                    "completed"
                };
                self.terminal = Some(status.into());
                self.usage = data.get("usage").filter(|v| v.is_object()).cloned();
                let part = json!({"type":"output_text","text":self.text,"annotations":[]});
                out.push(self.emit("response.output_text.done",json!({"item_id":self.item_id(),"output_index":0,"content_index":0,"text":self.text,"logprobs":[]})));
                out.push(self.emit("response.content_part.done",json!({"item_id":self.item_id(),"output_index":0,"content_index":0,"part":part})));
                out.push(self.emit("response.output_item.done",json!({"output_index":0,"item":{"id":self.item_id(),"type":"message","role":"assistant","status":if failed{"incomplete"}else{"completed"},"content":[part]}})));
                let error=failed.then(||json!({"code":status,"message":"The response could not be completed.","type":"server_error"}));
                let response = self.completed_response(error);
                out.push(self.emit(
                    if failed {
                        "response.failed"
                    } else {
                        "response.completed"
                    },
                    json!({"response":response}),
                ));
            }
            _ => {}
        }
        out
    }
    pub fn incomplete_event(&mut self) -> Option<Vec<u8>> {
        if self.terminal.is_some() {
            return None;
        }
        self.terminal = Some("agent_runtime_stream_incomplete".into());
        let response=self.completed_response(Some(json!({"code":"agent_runtime_stream_incomplete","message":"Agent Runtime ended without a terminal event.","type":"server_error"})));
        Some(self.emit("response.failed", json!({"response":response})))
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub digest: String,
}
pub enum BeginResponse {
    Unkeyed,
    Claim { scope: String, digest: String },
    Replay(CachedResponse),
}
struct MemoryEntry {
    expires: Instant,
    cached: Option<CachedResponse>,
}
pub struct ResponsesIdempotency {
    redis: Option<ConnectionManager>,
    memory: Arc<Mutex<HashMap<String, MemoryEntry>>>,
}
impl ResponsesIdempotency {
    pub fn new(redis: Option<ConnectionManager>) -> Self {
        Self {
            redis,
            memory: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    async fn cached(&self, key: &str) -> Result<Option<CachedResponse>, CompatError> {
        if let Some(mut redis) = self.redis.clone() {
            let raw = redis
                .get::<_, Option<String>>(format!("ai-gateway:responses:idem:response:{key}"))
                .await
                .map_err(|_| CompatError::new(503, "Idempotency store unavailable"))?;
            return raw
                .map(|v| {
                    serde_json::from_str(&v)
                        .map_err(|_| CompatError::new(503, "Invalid idempotency record"))
                })
                .transpose();
        }
        let mut memory = self.memory.lock().await;
        memory.retain(|_, entry| entry.expires > Instant::now());
        Ok(memory.get(key).and_then(|v| v.cached.clone()))
    }
    pub async fn begin(
        &self,
        user: &CompatUser,
        key: &str,
        body: &[u8],
    ) -> Result<BeginResponse, CompatError> {
        let key = key.trim();
        if key.is_empty() {
            return Ok(BeginResponse::Unkeyed);
        }
        if key.len() > 255 || !key.bytes().all(|c| (0x21..=0x7e).contains(&c)) {
            return Err(CompatError::new(400, "invalid_idempotency_key"));
        }
        let scope = hex(&Sha256::digest(
            format!("{}\n{}\n/v1/responses\n{key}", user.tenant_id, user.user_id).as_bytes(),
        ));
        let digest = hex(&Sha256::digest(body));
        let replay = |cached: CachedResponse| {
            if cached.digest != digest {
                Err(CompatError::new(409, "idempotency_key_conflict"))
            } else {
                Ok(BeginResponse::Replay(cached))
            }
        };
        if let Some(cached) = self.cached(&scope).await? {
            return replay(cached);
        }
        let begun = if let Some(mut redis) = self.redis.clone() {
            let result: Option<String> = redis::cmd("SET")
                .arg(format!("ai-gateway:responses:idem:lock:{scope}"))
                .arg("1")
                .arg("NX")
                .arg("EX")
                .arg(86_400)
                .query_async(&mut redis)
                .await
                .map_err(|_| CompatError::new(503, "Idempotency store unavailable"))?;
            result.is_some()
        } else {
            let mut memory = self.memory.lock().await;
            if memory.contains_key(&scope) {
                false
            } else {
                memory.insert(
                    scope.clone(),
                    MemoryEntry {
                        expires: Instant::now() + Duration::from_secs(86400),
                        cached: None,
                    },
                );
                true
            }
        };
        if begun {
            return Ok(BeginResponse::Claim { scope, digest });
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(cached) = self.cached(&scope).await? {
                return replay(cached);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Err(CompatError::new(409, "idempotency_request_in_progress"))
    }
    pub async fn finish(&self, scope: &str, cached: CachedResponse) -> Result<(), CompatError> {
        if cached.status >= 500 {
            return self.abort(scope).await;
        }
        if let Some(mut redis) = self.redis.clone() {
            let _: () = redis
                .set_ex(
                    format!("ai-gateway:responses:idem:response:{scope}"),
                    serde_json::to_string(&cached).map_err(|_| invalid("Invalid response"))?,
                    86400,
                )
                .await
                .map_err(|_| CompatError::new(503, "Idempotency store unavailable"))?;
        } else {
            self.memory.lock().await.insert(
                scope.into(),
                MemoryEntry {
                    expires: Instant::now() + Duration::from_secs(86400),
                    cached: Some(cached),
                },
            );
        }
        Ok(())
    }
    pub async fn abort(&self, scope: &str) -> Result<(), CompatError> {
        if let Some(mut redis) = self.redis.clone() {
            let _: usize = redis
                .del(format!("ai-gateway:responses:idem:lock:{scope}"))
                .await
                .map_err(|_| CompatError::new(503, "Idempotency store unavailable"))?;
        } else {
            self.memory.lock().await.remove(scope);
        }
        Ok(())
    }
}

use crate::{
    gateway_model_meta::MetadataModels,
    migration_agent::{
        Actor,
        control::{RuntimeControlPlane, TurnRequest},
    },
    rate_limits::{MultiDimensionConfig, RateLimitContext, SlidingWindowRateLimiter},
    session_manager::{CreateSession, SessionError},
    sessions_api::SessionsApiManager,
};
use ai_platform_auth::admission::AdmissionService;
use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{Request, State},
    http::{HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};

pub struct ResponsesState {
    pub auth: Arc<AdmissionService>,
    pub sessions: Arc<dyn SessionsApiManager>,
    pub control: Arc<RuntimeControlPlane>,
    pub models: Arc<dyn MetadataModels>,
    pub idempotency: ResponsesIdempotency,
    pub limiter: Option<Arc<SlidingWindowRateLimiter>>,
    pub rate_config: MultiDimensionConfig,
}
pub fn router(state: Arc<ResponsesState>) -> Router {
    Router::new()
        .route("/v1/responses", post(create_response))
        .route("/api/v1/responses", post(create_response))
        .with_state(state)
}
pub fn response_error(
    status: u16,
    code: &str,
    message: &str,
    param: Option<&str>,
    kind: &str,
) -> Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(json!({"error":{"message":message,"type":kind,"param":param,"code":code}})),
    )
        .into_response()
}
async fn create_response(State(state): State<Arc<ResponsesState>>, request: Request) -> Response {
    let admission = state.auth.request(request.headers(), request.uri().path());
    let mut user = match admission.resolve().await {
        Ok(principal) => CompatUser::from_admitted(principal, request.headers()),
        Err(_) => {
            return response_error(
                401,
                "authentication_required",
                "Authentication and tenant identity are required.",
                None,
                "authentication_error",
            );
        }
    };
    user.apply_request_context(request.extensions());
    if !user.authenticated
        || user.user_id.is_empty()
        || user.tenant_id.is_empty()
        || user.user_id == "anonymous"
        || user.tenant_id == "public"
    {
        return response_error(
            401,
            "authentication_required",
            "Authentication and tenant identity are required.",
            None,
            "authentication_error",
        );
    }
    drop(admission);
    if request.uri().query().is_some_and(|v| !v.is_empty()) {
        return response_error(
            400,
            "unsupported_query_parameters",
            "Query parameters are not supported for this endpoint.",
            None,
            "invalid_request_error",
        );
    }
    if let Some(limiter) = &state.limiter {
        let context = RateLimitContext {
            user_id: Some(user.user_id.clone()),
            tenant_id: Some(user.tenant_id.clone()),
            user_tier: user.tier.clone(),
            operation: Some("assistant_chat".into()),
            ..Default::default()
        };
        let result = match state.rate_config.policies(
            &context,
            &request
                .extensions()
                .get::<crate::migration_host::rate::CountedDimensions>()
                .map(|v| v.0.clone())
                .unwrap_or_default(),
        ) {
            Ok(policies) => limiter.check_http_many(&policies).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(result) if !result.result.allowed => {
                let mut response = response_error(
                    429,
                    "rate_limit_exceeded",
                    "Rate limit exceeded.",
                    None,
                    "rate_limit_error",
                );
                for (k, v) in result.result.headers() {
                    if let (Ok(k), Ok(v)) = (
                        HeaderName::from_bytes(k.as_bytes()),
                        HeaderValue::from_str(&v),
                    ) {
                        response.headers_mut().insert(k, v);
                    }
                }
                return response;
            }
            Err(_) => {
                return response_error(
                    503,
                    "rate_limit_unavailable",
                    "Rate limiting is temporarily unavailable.",
                    None,
                    "server_error",
                );
            }
            _ => {}
        }
    }
    let idempotency_key = super::common::header(request.headers(), "idempotency-key").to_owned();
    let forged_header = request.headers().keys().any(|key| {
        key.as_str().starts_with("x-agent-")
            && !matches!(key.as_str(), "x-agent-embed-token" | "x-agent-embed-origin")
    });
    let bytes = match to_bytes(request.into_body(), 16 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return response_error(
                413,
                "request_too_large",
                "Request body is too large.",
                None,
                "invalid_request_error",
            );
        }
    };
    let payload: Value = match serde_json::from_slice(&bytes) {
        Ok(payload) => payload,
        Err(_) => {
            return response_error(
                400,
                "invalid_json",
                "Invalid JSON body.",
                None,
                "invalid_request_error",
            );
        }
    };
    if !payload.is_object() {
        return response_error(
            400,
            "invalid_json_object",
            "Request body must be a JSON object.",
            None,
            "invalid_request_error",
        );
    }
    let forged_body = [
        "agent_id",
        "agent_version_id",
        "draft_revision",
        "publication_id",
        "channel",
        "resolved_snapshot",
        "runtime_envelope",
        "snapshot_hash",
        "spec_hash",
        "runtime_fingerprint",
    ]
    .iter()
    .any(|field| payload.get(*field).is_some());
    if forged_header || forged_body {
        return response_error(
            if forged_header { 400 } else { 422 },
            "agent_runtime_field_forbidden",
            "Client-supplied Agent runtime fields or headers are forbidden.",
            None,
            "invalid_request_error",
        );
    }
    let model = match payload["model"]
        .as_str()
        .filter(|v| !v.trim().is_empty() && v.chars().count() <= 255)
    {
        Some(model) => model,
        None => {
            return response_error(
                400,
                "invalid_model",
                "model must be a non-empty string.",
                Some("model"),
                "invalid_request_error",
            );
        }
    };
    if payload
        .get("expected_model_ref")
        .is_some_and(|v| !v.is_null() && !v.is_object())
    {
        return response_error(
            400,
            "invalid_expected_model_ref",
            "expected_model_ref must be an object.",
            Some("expected_model_ref"),
            "invalid_request_error",
        );
    }
    if payload.get("provider_id").is_some_and(|v| {
        !v.is_null()
            && v.as_str()
                .is_none_or(|v| v.trim().is_empty() || v.chars().count() > 255)
    }) {
        return response_error(
            400,
            "invalid_provider_id",
            "provider_id must be a non-empty string.",
            Some("provider_id"),
            "invalid_request_error",
        );
    }
    let provider = payload["provider_id"].as_str().map(str::trim);
    let model_record = match state
        .models
        .get_model(&user.tenant_id, model, provider)
        .await
    {
        Ok(Some(record)) => Value::Object(record),
        Ok(None) => {
            return response_error(
                400,
                "model_not_found",
                "The requested model was not found.",
                Some("model"),
                "invalid_request_error",
            );
        }
        Err(crate::model_service::ModelServiceError::ProviderAmbiguous) => {
            return response_error(
                409,
                "model_provider_required",
                "Specify provider_id for this model.",
                Some("provider_id"),
                "invalid_request_error",
            );
        }
        Err(_) => {
            return response_error(
                503,
                "model_authorization_unavailable",
                "Model authorization is temporarily unavailable.",
                Some("model"),
                "server_error",
            );
        }
    };
    let actor = Actor {
        tenant_id: user.tenant_id.clone(),
        user_id: user.user_id.clone(),
        roles: user.roles.clone(),
        tier: user.tier.clone(),
    };
    if !crate::migration_agent::entry::user_can_access_model(
        &actor,
        text(&model_record, "access_level"),
    ) {
        return response_error(
            403,
            "model_access_denied",
            "Access to the requested model is denied.",
            Some("model"),
            "permission_error",
        );
    }
    let parsed = match ResponsesRequest::after_model_authorization(payload, &idempotency_key) {
        Ok(parsed) => parsed,
        Err(error) => {
            let detail = error.detail.as_str().unwrap_or("");
            let code = error
                .detail
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    if detail.contains("tool") {
                        "invalid_tools"
                    } else if detail.contains("instructions") {
                        "invalid_instructions"
                    } else if detail.contains("reasoning") {
                        "invalid_reasoning"
                    } else if detail.contains("max_output_tokens") {
                        "invalid_max_output_tokens"
                    } else if detail.contains("temperature") {
                        "invalid_temperature"
                    } else if detail.contains("stream") {
                        "invalid_stream"
                    } else {
                        "invalid_input"
                    }
                });
            return response_error(
                error.status,
                code,
                if detail.is_empty() {
                    "The Responses request is not supported by the Agent Runtime."
                } else {
                    detail
                },
                None,
                "invalid_request_error",
            );
        }
    };
    let claim = match state
        .idempotency
        .begin(&user, &idempotency_key, &bytes)
        .await
    {
        Ok(BeginResponse::Replay(cached)) => {
            let mut response = (
                StatusCode::from_u16(cached.status).unwrap_or(StatusCode::OK),
                cached.body,
            )
                .into_response();
            for (k, v) in cached.headers {
                if !matches!(
                    k.to_lowercase().as_str(),
                    "content-length" | "transfer-encoding"
                ) {
                    if let (Ok(k), Ok(v)) = (
                        HeaderName::from_bytes(k.as_bytes()),
                        HeaderValue::from_str(&v),
                    ) {
                        response.headers_mut().insert(k, v);
                    }
                }
            }
            response
                .headers_mut()
                .insert("x-idempotency-replayed", HeaderValue::from_static("true"));
            return response;
        }
        Ok(BeginResponse::Claim { scope, digest }) => Some((scope, digest)),
        Ok(BeginResponse::Unkeyed) => None,
        Err(error) => {
            return response_error(
                error.status,
                error.detail.as_str().unwrap_or("idempotency_failed"),
                "The idempotent request could not proceed.",
                None,
                "invalid_request_error",
            );
        }
    };
    let scope = actor.scope(&parsed.session_id);
    let prepare_session = async {
        if let Some(session) = state
            .sessions
            .get(&parsed.session_id)
            .await
            .map_err(|_| CompatError::new(503, "Session storage unavailable"))?
        {
            if session.user_id != user.user_id || session.tenant_id != user.tenant_id {
                return Err(CompatError::new(404, "Session not found"));
            }
        } else {
            match state
                .sessions
                .create(CreateSession {
                    user_id: user.user_id.clone(),
                    tenant_id: user.tenant_id.clone(),
                    service_id: Some("__builtin_assistant__".into()),
                    session_id: Some(parsed.session_id.clone()),
                    fail_if_exists: true,
                    ..Default::default()
                })
                .await
            {
                Ok(_) => {}
                Err(_) => {
                    let session = state
                        .sessions
                        .get(&parsed.session_id)
                        .await
                        .map_err(|_| CompatError::new(503, "Session storage unavailable"))?;
                    if session
                        .is_none_or(|s| s.user_id != user.user_id || s.tenant_id != user.tenant_id)
                    {
                        return Err(CompatError::new(404, "Session not found"));
                    }
                }
            }
        }
        state
            .control
            .threads
            .bind_new_assignment(&scope, &state.control.options.kernel_revision)
            .await
            .map_err(|e| CompatError::value(e.status, e.detail))?;
        Ok::<_, CompatError>(())
    }
    .await;
    if prepare_session.is_err() {
        if let Some((scope, _)) = &claim {
            let _ = state.idempotency.abort(scope).await;
        }
        return response_error(
            503,
            "agent_runtime_session_unavailable",
            "Agent Runtime session storage is temporarily unavailable.",
            None,
            "server_error",
        );
    }
    let turn = state
        .control
        .start_turn(TurnRequest {
            actor,
            session_id: parsed.session_id.clone(),
            message: parsed.message.clone(),
            model_id: parsed.model.clone(),
            provider_id: parsed.provider_id.clone(),
            expected_model_ref: parsed
                .payload
                .get("expected_model_ref")
                .filter(|v| !v.is_null())
                .cloned(),
            reasoning_option: parsed.payload["reasoning"]["effort"]
                .as_str()
                .map(str::to_owned),
            max_tokens: parsed.payload["max_output_tokens"].as_i64(),
            temperature: parsed.payload["temperature"].as_f64(),
            developer_instructions: parsed.payload["instructions"].as_str().map(str::to_owned),
            readonly_capabilities: parsed.readonly_capabilities(),
            memory_mode: "off".into(),
            memory_profile: Some("off".into()),
            enable_dynamic_tools: parsed.tools["tool_names"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
                && parsed.tools["tool_choice"] != json!("none"),
            ..Default::default()
        })
        .await;
    let turn = match turn {
        Ok(turn) => turn,
        Err(error) => {
            if let Some((scope, _)) = &claim {
                let _ = state.idempotency.abort(scope).await;
            }
            return response_error(
                error.status,
                &text(&error.detail, "code").to_lowercase(),
                "Agent Runtime rejected the request.",
                None,
                if error.status >= 500 {
                    "server_error"
                } else {
                    "invalid_request_error"
                },
            );
        }
    };
    let mut projection = ResponseProjection::new(&turn.run_id, &parsed);
    let events = state
        .control
        .stream_thread_events(
            scope,
            turn.runtime_thread_id,
            turn.after_sequence,
            1000,
            Some(turn.run_id),
        )
        .await;
    let mut events = match events {
        Ok(events) => events,
        Err(_) => {
            if let Some((scope, _)) = &claim {
                let _ = state.idempotency.abort(scope).await;
            }
            return response_error(
                503,
                "agent_runtime_event_stream_failed",
                "Agent Runtime event stream is temporarily unavailable.",
                None,
                "server_error",
            );
        }
    };
    if parsed.stream {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            for bytes in projection.initial_events() {
                if tx.send(Ok(Bytes::from(bytes))).await.is_err() {
                    return;
                }
            }
            loop {
                tokio::select! {_=tx.closed()=>return,event=events.recv()=>match event{Some(Ok(event))=>{for bytes in projection.observe(&event){if tx.send(Ok(Bytes::from(bytes))).await.is_err(){return;}}if projection.terminal.is_some(){return;}},Some(Err(_))=>{let _=tx.send(Err(std::io::Error::other("Agent Runtime event stream interrupted"))).await;return;},None=>break}}
            }
            if let Some(bytes) = projection.incomplete_event() {
                let _ = tx.send(Ok(Bytes::from(bytes))).await;
            }
        });
        return (
            [
                ("content-type", "text/event-stream"),
                ("cache-control", "no-cache"),
                ("x-accel-buffering", "no"),
            ],
            Body::from_stream(super::common::ByteReceiver(rx)),
        )
            .into_response();
    }
    while let Some(event) = events.recv().await {
        match event {
            Ok(event) => {
                projection.observe(&event);
                if projection.terminal.is_some() {
                    break;
                }
            }
            Err(_) => {
                if let Some((scope, _)) = &claim {
                    let _ = state.idempotency.abort(scope).await;
                }
                return response_error(
                    503,
                    "agent_runtime_event_stream_failed",
                    "Agent Runtime event stream is temporarily unavailable.",
                    None,
                    "server_error",
                );
            }
        }
    }
    if projection.terminal.is_none() {
        if let Some((scope, _)) = &claim {
            let _ = state.idempotency.abort(scope).await;
        }
        return response_error(
            502,
            "agent_runtime_stream_incomplete",
            "Agent Runtime ended without a terminal event.",
            None,
            "server_error",
        );
    }
    let error=projection.terminal.as_deref().filter(|status|*status!="completed").map(|status|json!({"code":status,"message":"The response could not be completed.","type":"server_error"}));
    let payload = projection.completed_response(error);
    if let Some((scope, digest)) = claim {
        let cached = CachedResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: payload.to_string().into_bytes(),
            digest,
        };
        if state.idempotency.finish(&scope, cached).await.is_err() {
            return response_error(
                503,
                "idempotency_store_unavailable",
                "Response persistence is temporarily unavailable.",
                None,
                "server_error",
            );
        }
    }
    Json(payload).into_response()
}
