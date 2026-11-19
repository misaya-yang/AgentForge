//! Source port of `src/adapters/generic_rest.py` with an injectable connector.
//!
//! Sync invocation, mapping precedence, literal expressions, JSONPath cardinality,
//! output defaults, and raw-response fallback follow the Python adapter. Stream,
//! submit and get_result retain the base adapter's unsupported behavior. No Agent
//! loop, provider selection, retry or detached request is added. The HTTP client
//! retains ordinary proxy-environment behavior, as does the Python connector.
//!
//! JSONPath follows the pinned base jsonpath-ng 1.8.0 grammar, including
//! recursive descent, expression grouping/unions, existence predicates,
//! named this/parent operators, numeric fields and Unicode string indices.
//! Source intersection is deliberately an evaluation error (NotImplementedError).
//! Arbitrary Python objects and request datetime/bytes selections fail closed;
//! datetime/bytes are never silently converted to strings/arrays.
//!
//! The optional reqwest connector ports HTTP URL/header/status/JSON/health logic.
//! It disables redirects and automatic retries. Its I/O runs on the caller task;
//! cancellation drops the future, response and capacity permit. Health is true
//! for every received status below 500, including 403, exactly as HTTPConnector.
//! Transport gaps: reqwest cannot reproduce httpx's independent write timeout or
//! global idle-connection accounting (idle limit is per-host). The dispatcher's
//! call timeout remains authoritative. Configured HTTP/2 uses ordinary TLS ALPN
//! negotiation. JSON bytes support UTF-8/16/32 detection; text uses declared
//! encodings with replacement, matching httpx for supported codecs. Python-only
//! codecs remain outside the port. Scheme-relative endpoints are rejected.
//! Connect timeouts use nonretryable Other, matching Python ConnectTimeout;
//! other send timeouts remain ambiguous. Ambiguous send errors use WriteError,
//! never a fabricated retryable RemoteProtocolError. Errors never include URLs,
//! credentials, mapped data or upstream response text.
//!
//! Live constructors retain the original nonempty connector dictionary. Request
//! and response mapping values are captured once, as the source captures those
//! dictionary objects; transport settings are also fixed at construction. The
//! current ownership model does not expose independent nested JSON-object handles:
//! in-place edits inside an already captured mapping are not live. Replacing a
//! mapping key does not replace the adapter's captured mapping. Reachable service
//! normalization mutates URL/graph/model_override keys, not these nested mappings.
//! HTTP health checks reread the captured service's connector at each check.
use crate::{
    dispatcher::{AdapterFailure, DispatchError},
    protocol::{
        ContentData, ContentItem, ContentType, ServiceDefinition, TransportType, UnifiedRequest,
        UnifiedResponse,
    },
    service_ownership::{ConnectorHandle, ServiceHandle},
    service_registry::{
        AdapterError, AdapterFactory, AdapterFuture, ProtocolAdapter, RegistryError, python_truthy,
    },
};
use reqwest::{
    Client, Method, Url,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use serde_json::{Map, Value, json};
use std::{fmt, future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestMappingError {
    InvalidConfig(&'static str),
    InvalidMapping(&'static str),
    RequestBodyMustBeObject,
    UnsupportedJsonPath,
    JsonPathEvaluation,
    NonJsonRequestValue,
    InvalidOutput,
}
impl fmt::Display for RestMappingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfig(message) | Self::InvalidMapping(message) => message,
            Self::RequestBodyMustBeObject => "request_mapping.body must be a dict",
            Self::UnsupportedJsonPath => "unsupported_generic_rest_jsonpath",
            Self::JsonPathEvaluation => "generic_rest_jsonpath_evaluation_failed",
            Self::NonJsonRequestValue => "generic_rest_non_json_request_value",
            Self::InvalidOutput => "invalid_generic_rest_output",
        })
    }
}
impl std::error::Error for RestMappingError {}
impl From<RestMappingError> for AdapterError {
    fn from(error: RestMappingError) -> Self {
        match error {
            RestMappingError::RequestBodyMustBeObject => Self::Dispatch(
                DispatchError::AdapterValidationFailed("request_mapping.body must be a dict"),
            ),
            _ => AdapterFailure::Other.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RestRequest {
    pub method: String,
    pub endpoint: String,
    pub body: Value,
}
/// HTTPConnector already decoded application/json before returning to the adapter.
/// Other content types expose response.json() plus response.text to the adapter.
#[derive(Clone, Debug, PartialEq)]
pub enum RestResponse {
    Json(Value),
    HttpText(String),
}
pub type RestTransportFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, AdapterFailure>> + Send + 'a>>;
/// Implementations must perform I/O in this future, never spawn detached work.
pub trait RestTransport: Send + Sync {
    fn request(&self, request: RestRequest) -> RestTransportFuture<'_, RestResponse>;
    fn health_check(&self) -> RestTransportFuture<'_, bool>;
    fn shutdown(&self) -> RestTransportFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

pub struct GenericRestAdapter {
    config: ConnectorHandle,
    request_mapping: Value,
    response_mapping: Value,
    transport: Arc<dyn RestTransport>,
}
impl GenericRestAdapter {
    /// DTO convenience constructor; subsequent DTO edits are detached.
    /// Injection also permits a separately implemented non-HTTP connector.
    pub fn with_transport(
        service: &ServiceDefinition,
        transport: Arc<dyn RestTransport>,
    ) -> Result<Self, RestMappingError> {
        Self::with_transport_handle(&ServiceHandle::new(service.clone()), transport)
    }
    /// Retain the selected live object's connector, including its source `or {}`
    /// behavior for an empty dictionary. Do not reacquire by registry service ID.
    pub fn with_transport_handle(
        service: &ServiceHandle,
        transport: Arc<dyn RestTransport>,
    ) -> Result<Self, RestMappingError> {
        let connector = service.connector();
        let value = connector.snapshot();
        if python_truthy(&value) && !value.is_object() {
            return Err(RestMappingError::InvalidConfig("invalid_connector_config"));
        }
        let config = connector.dictionary_or_empty();
        let captured = config.snapshot();
        Ok(Self {
            request_mapping: captured
                .get("request_mapping")
                .cloned()
                .unwrap_or_else(|| json!({})),
            response_mapping: captured
                .get("response_mapping")
                .cloned()
                .unwrap_or_else(|| json!({})),
            config,
            transport,
        })
    }
    pub fn new_http(service: &ServiceDefinition) -> Result<Self, RestMappingError> {
        Self::new_http_handle(&ServiceHandle::new(service.clone()))
    }
    pub fn new_http_handle(service: &ServiceHandle) -> Result<Self, RestMappingError> {
        if service.snapshot().connector_type != TransportType::Http {
            return Err(RestMappingError::InvalidConfig(
                "generic_rest_requires_http_transport",
            ));
        }
        Self::with_transport_handle(
            service,
            Arc::new(ReqwestRestTransport::new_handle(service)?),
        )
    }
    fn mapping(value: &Value) -> Result<Map<String, Value>, RestMappingError> {
        if !python_truthy(value) {
            return Ok(Map::new());
        }
        value
            .as_object()
            .cloned()
            .ok_or(RestMappingError::InvalidMapping(
                "generic_rest_mapping_must_be_object",
            ))
    }
    pub fn build_service_request(
        &self,
        request: &UnifiedRequest,
    ) -> Result<RestRequest, RestMappingError> {
        let mapping = Self::mapping(&self.request_mapping)?;
        let config = self.config.snapshot();
        let choose = |key, default: &str| -> Result<String, RestMappingError> {
            mapping
                .get(key)
                .filter(|v| python_truthy(v))
                .or_else(|| config.get(key).filter(|v| python_truthy(v)))
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or(RestMappingError::InvalidMapping(
                            "generic_rest_endpoint_and_method_must_be_strings",
                        ))
                })
                .unwrap_or_else(|| Ok(default.into()))
        };
        let endpoint = choose("endpoint", "/")?;
        let method = choose("method", "POST")?;
        let body_map = match mapping.get("body").filter(|v| python_truthy(v)) {
            None => Map::new(),
            Some(value) => value
                .as_object()
                .cloned()
                .ok_or(RestMappingError::RequestBodyMustBeObject)?,
        };
        let data =
            serde_json::to_value(request).map_err(|_| RestMappingError::NonJsonRequestValue)?;
        // Preserve the source's unconverted datetime / bytes instead of quietly
        // accepting serde's timestamp string and byte-array representation.
        let mut opaque = vec![&data["timestamp"]];
        for (index, item) in request.inputs.iter().enumerate() {
            if matches!(item.data, Some(ContentData::Bytes(_))) {
                opaque.push(&data["inputs"][index]["data"]);
            }
        }
        let mut body = Map::new();
        for (key, expr) in body_map {
            body.insert(key, evaluate(&expr, &data, &opaque)?);
        }
        Ok(RestRequest {
            method,
            endpoint,
            body: Value::Object(body),
        })
    }
    pub fn build_outputs(
        &self,
        response: RestResponse,
    ) -> Result<Vec<ContentItem>, RestMappingError> {
        let mapping = Self::mapping(&self.response_mapping)?;
        let response = match response {
            RestResponse::Json(data) => data,
            RestResponse::HttpText(text) => {
                serde_json::from_str(&text).unwrap_or_else(|_| json!({"raw":text}))
            }
        };
        let Some(specs) = mapping.get("outputs").filter(|v| python_truthy(v)) else {
            return Ok(vec![content(ContentType::Json, response)]);
        };
        let specs = specs.as_array().ok_or(RestMappingError::InvalidOutput)?;
        specs
            .iter()
            .map(|spec| {
                let spec = spec.as_object().ok_or(RestMappingError::InvalidOutput)?;
                // Serde also accepts externally tagged maps for unit enums, but
                // Python ContentType({"image": None}) raises ValueError.
                let type_name = match spec.get("type") {
                    None => "text",
                    Some(Value::String(name)) => name.as_str(),
                    _ => return Err(RestMappingError::InvalidOutput),
                };
                let content_type = serde_json::from_value(Value::String(type_name.into()))
                    .map_err(|_| RestMappingError::InvalidOutput)?;
                let eval = |key| -> Result<Value, RestMappingError> {
                    spec.get(key)
                        .filter(|v| python_truthy(v))
                        .map(|v| evaluate(v, &response, &[]))
                        .unwrap_or(Ok(Value::Null))
                };
                let mut item = content(content_type, eval("data")?);
                item.url = optional_string(eval("url")?)?;
                item.mime_type =
                    optional_string(spec.get("mime_type").cloned().unwrap_or_default())?;
                item.metadata = match spec.get("metadata") {
                    None | Some(Value::Null) => None,
                    Some(Value::Object(value)) => Some(value.clone()),
                    _ => return Err(RestMappingError::InvalidOutput),
                };
                Ok(item)
            })
            .collect()
    }
}
impl ProtocolAdapter for GenericRestAdapter {
    fn invoke<'a>(&'a self, request: &'a UnifiedRequest) -> AdapterFuture<'a, UnifiedResponse> {
        Box::pin(async move {
            let mapped = self.build_service_request(request)?;
            let response = self.transport.request(mapped).await?;
            Ok(UnifiedResponse::new(
                &request.request_id,
                "success",
                self.build_outputs(response)?,
            ))
        })
    }
    fn health_check(&self) -> AdapterFuture<'_, bool> {
        Box::pin(async { Ok(self.transport.health_check().await.unwrap_or(false)) })
    }
    fn shutdown(&self) -> AdapterFuture<'_, ()> {
        Box::pin(async { self.transport.shutdown().await.map_err(Into::into) })
    }
}
#[derive(Default)]
pub struct GenericRestAdapterFactory;
impl AdapterFactory for GenericRestAdapterFactory {
    fn create(&self, service: ServiceHandle) -> Result<Arc<dyn ProtocolAdapter>, RegistryError> {
        let adapter = if service.snapshot().connector_type == TransportType::MessageQueue {
            crate::message_queue_transport::MessageQueueTransport::new_handle(&service).and_then(
                |transport| {
                    GenericRestAdapter::with_transport_handle(&service, Arc::new(transport))
                },
            )
        } else {
            GenericRestAdapter::new_http_handle(&service)
        };
        adapter
            .map(|a| Arc::new(a) as Arc<dyn ProtocolAdapter>)
            .map_err(|_| RegistryError::AdapterConstructionFailed)
    }
}
fn optional_string(value: Value) -> Result<Option<String>, RestMappingError> {
    match value {
        Value::Null => Ok(None),
        Value::String(value) => Ok(Some(value)),
        _ => Err(RestMappingError::InvalidOutput),
    }
}
fn content(content_type: ContentType, data: Value) -> ContentItem {
    ContentItem {
        content_type,
        data: match data {
            Value::Null => None,
            Value::String(s) => Some(ContentData::Text(s)),
            value => Some(ContentData::Json(value)),
        },
        url: None,
        mime_type: None,
        metadata: None,
    }
}

/// Evaluates literal values without recursively interpolating their contents.
pub fn evaluate_expression(expr: &Value, data: &Value) -> Result<Value, RestMappingError> {
    evaluate(expr, data, &[])
}
fn contains_opaque(value: &Value, opaque: &[&Value]) -> bool {
    opaque.iter().any(|item| std::ptr::eq(value, *item))
        || match value {
            Value::Array(items) => items.iter().any(|v| contains_opaque(v, opaque)),
            Value::Object(items) => items.values().any(|v| contains_opaque(v, opaque)),
            _ => false,
        }
}
#[path = "generic_rest_jsonpath.rs"]
mod extended_jsonpath;
fn evaluate(expr: &Value, data: &Value, opaque: &[&Value]) -> Result<Value, RestMappingError> {
    match evaluate_basic(expr, data, opaque) {
        Err(RestMappingError::UnsupportedJsonPath | RestMappingError::JsonPathEvaluation) => {
            if let Some(path) = expr.as_str().filter(|s| s.trim().starts_with('$')) {
                extended_jsonpath::evaluate(path, data, opaque)
            } else {
                Ok(expr.clone())
            }
        }
        result => result,
    }
}
fn evaluate_basic(
    expr: &Value,
    data: &Value,
    opaque: &[&Value],
) -> Result<Value, RestMappingError> {
    let Some(path) = expr.as_str().filter(|expr| expr.trim().starts_with('$')) else {
        return Ok(expr.clone());
    };
    let selectors = Parser::new(path).parse()?;
    let mut values = vec![data];
    for selector in selectors {
        let mut selected = Vec::new();
        for value in values {
            if opaque.iter().any(|item| std::ptr::eq(value, *item)) {
                return Err(RestMappingError::NonJsonRequestValue);
            }
            selector.select(value, &mut selected, opaque)?;
        }
        values = selected;
    }
    if values.iter().any(|value| contains_opaque(value, opaque)) {
        return Err(RestMappingError::NonJsonRequestValue);
    }
    Ok(match values.len() {
        0 => Value::Null,
        1 => values[0].clone(),
        _ => Value::Array(values.into_iter().cloned().collect()),
    })
}
#[derive(Debug)]
enum Selector {
    Fields(Vec<String>),
    Indices(Vec<i64>),
    Slice(Option<i64>, Option<i64>, i64),
    Descendants(Vec<Selector>),
}
impl Selector {
    fn select<'a>(
        &self,
        value: &'a Value,
        output: &mut Vec<&'a Value>,
        opaque: &[&Value],
    ) -> Result<(), RestMappingError> {
        if opaque.iter().any(|item| std::ptr::eq(value, *item)) {
            return Err(RestMappingError::NonJsonRequestValue);
        }
        match self {
            Self::Descendants(selectors) => {
                // Match at this node before its children. Never use Slice for
                // traversal: its scalar coercion would recurse indefinitely.
                let mut pending = vec![value];
                while let Some(node) = pending.pop() {
                    let mut matches = vec![node];
                    for selector in selectors {
                        let mut selected = Vec::new();
                        for matched in matches {
                            selector.select(matched, &mut selected, opaque)?;
                        }
                        matches = selected;
                    }
                    output.extend(matches);
                    match node {
                        Value::Array(items) => pending.extend(items.iter().rev()),
                        Value::Object(items) => pending.extend(items.values().rev()),
                        _ => {}
                    }
                }
            }
            Self::Fields(fields) => {
                if let Value::Object(object) = value {
                    if fields.iter().any(|f| f == "*") {
                        output.extend(object.values());
                    } else {
                        output.extend(fields.iter().filter_map(|field| object.get(field)));
                    }
                }
            }
            Self::Indices(indices) => {
                if !python_truthy(value) {
                    return Ok(());
                }
                // jsonpath-ng checks truthiness and length before indexing.
                // A large index on an object/string is a non-match, even though
                // an in-range object index fails; strings use the contextual fallback.
                let length = match value {
                    Value::Array(items) => items.len(),
                    Value::Object(items) => items.len(),
                    Value::String(text) => text.chars().count(),
                    _ => return Err(RestMappingError::JsonPathEvaluation),
                };
                for &index in indices {
                    if length as i128 <= index as i128 {
                        continue;
                    }
                    let array = value
                        .as_array()
                        .ok_or(RestMappingError::JsonPathEvaluation)?;
                    let actual = if index < 0 {
                        array.len() as i128 + index as i128
                    } else {
                        index as i128
                    };
                    if actual < 0 {
                        return Err(RestMappingError::JsonPathEvaluation);
                    }
                    if let Ok(index) = usize::try_from(actual)
                        && let Some(item) = array.get(index)
                    {
                        output.push(item);
                    }
                }
            }
            Self::Slice(start, end, step) => {
                if value.is_null() {
                    return Ok(());
                }
                if *step == 0 {
                    return Err(RestMappingError::JsonPathEvaluation);
                }
                let values = if let Value::Array(values) = value {
                    values.as_slice()
                } else if value.is_object()
                    || value.is_string()
                    || value.is_boolean()
                    || value.is_number()
                {
                    // Only the scalar's position is sliced; return the original
                    // reference to retain opaque request-value provenance.
                    if slice_indices(1, *start, *end, *step).next().is_some() {
                        output.push(value);
                    }
                    return Ok(());
                } else {
                    return Err(RestMappingError::JsonPathEvaluation);
                };
                output.extend(
                    slice_indices(values.len(), *start, *end, *step).map(|index| &values[index]),
                );
            }
        }
        Ok(())
    }
}
fn slice_indices(
    len: usize,
    start: Option<i64>,
    end: Option<i64>,
    step: i64,
) -> impl Iterator<Item = usize> {
    let len = len as i128;
    let step = step as i128;
    let bound = |index: i64| {
        let index = index as i128;
        let index = if index < 0 { len + index } else { index };
        if step < 0 {
            index.clamp(-1, len - 1)
        } else {
            index.clamp(0, len)
        }
    };
    let mut index = start
        .map(bound)
        .unwrap_or(if step < 0 { len - 1 } else { 0 });
    let end = end.map(bound).unwrap_or(if step < 0 { -1 } else { len });
    std::iter::from_fn(move || {
        if (step > 0 && index >= end) || (step < 0 && index <= end) {
            return None;
        }
        let result = index as usize;
        index += step;
        Some(result)
    })
}
struct Parser<'a> {
    text: &'a str,
}
impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Self { text }
    }
    fn space(&mut self) {
        self.text = self.text.trim_start_matches([' ', '\t', '\r', '\n']);
    }
    fn take(&mut self, token: char) -> bool {
        self.space();
        if let Some(rest) = self.text.strip_prefix(token) {
            self.text = rest;
            true
        } else {
            false
        }
    }
    fn parse(mut self) -> Result<Vec<Selector>, RestMappingError> {
        if !self.take('$') {
            return Err(RestMappingError::UnsupportedJsonPath);
        }
        let mut selectors = Vec::new();
        let mut descendants: Option<Vec<Selector>> = None;
        loop {
            self.space();
            if self.text.is_empty() {
                if let Some(group) = descendants.take() {
                    selectors.push(Selector::Descendants(group));
                }
                return Ok(selectors);
            }
            let dot = self.take('.');
            let recursive = dot && self.take('.');
            if recursive {
                if let Some(group) = descendants.take() {
                    selectors.push(Selector::Descendants(group));
                }
                descendants = Some(Vec::new());
            } else if !dot {
                // An implicit bracket child reduces the preceding descent,
                // unlike an explicit dot child (jsonpath-ng precedence).
                if let Some(group) = descendants.take() {
                    selectors.push(Selector::Descendants(group));
                }
            }
            let selector = if self.take('[') {
                self.bracket()?
            } else if dot {
                Selector::Fields(self.fields()?)
            } else {
                return Err(RestMappingError::UnsupportedJsonPath);
            };
            if let Some(group) = descendants.as_mut() {
                group.push(selector);
            } else {
                selectors.push(selector);
            }
        }
    }
    fn field(&mut self) -> Result<String, RestMappingError> {
        self.space();
        if let Some(quote @ ('\'' | '"')) = self.text.chars().next() {
            self.text = &self.text[1..];
            let mut value = String::new();
            loop {
                let ch = self
                    .text
                    .chars()
                    .next()
                    .ok_or(RestMappingError::UnsupportedJsonPath)?;
                self.text = &self.text[ch.len_utf8()..];
                if ch == quote {
                    return Ok(value);
                }
                if ch == '\\' {
                    let ch = self
                        .text
                        .chars()
                        .next()
                        .ok_or(RestMappingError::UnsupportedJsonPath)?;
                    self.text = &self.text[ch.len_utf8()..];
                    value.push(ch);
                } else {
                    value.push(ch);
                }
            }
        }
        if self.take('*') {
            return Ok("*".into());
        }
        let first = |ch: char| {
            ch.is_ascii_alphabetic()
                || matches!(ch, '_' | '@' | '\u{4e00}'..='\u{9fa5}' | '\u{1f600}'..='\u{1f64f}')
        };
        let mut end = 0;
        for (index, ch) in self.text.char_indices() {
            if !(first(ch) || (index > 0 && (ch.is_ascii_digit() || ch == '-'))) {
                break;
            }
            end = index + ch.len_utf8();
        }
        if end == 0 {
            return Err(RestMappingError::UnsupportedJsonPath);
        }
        let field = self.text[..end].to_owned();
        self.text = &self.text[end..];
        if matches!(field.as_str(), "where" | "wherenot") {
            return Err(RestMappingError::UnsupportedJsonPath);
        }
        Ok(field)
    }
    fn fields(&mut self) -> Result<Vec<String>, RestMappingError> {
        self.space();
        let wildcard = self.text.starts_with('*');
        let mut fields = vec![self.field()?];
        while self.take(',') {
            self.space();
            if wildcard || self.text.starts_with('*') {
                return Err(RestMappingError::UnsupportedJsonPath);
            }
            fields.push(self.field()?);
        }
        Ok(fields)
    }
    fn integer(&mut self) -> Result<Option<i64>, RestMappingError> {
        self.space();
        let count = self
            .text
            .bytes()
            .enumerate()
            .take_while(|(i, b)| b.is_ascii_digit() || (*i == 0 && *b == b'-'))
            .count();
        if count == 0 {
            return Ok(None);
        }
        let token = &self.text[..count];
        if token == "-" {
            return Err(RestMappingError::UnsupportedJsonPath);
        }
        // Python indices are arbitrary-size integers; bounds beyond any native
        // collection are semantically saturated, never parse failures.
        let value = token.parse().unwrap_or(if token.starts_with('-') {
            i64::MIN
        } else {
            i64::MAX
        });
        self.text = &self.text[count..];
        Ok(Some(value))
    }
    fn bracket(&mut self) -> Result<Selector, RestMappingError> {
        let selector = if self.take('*') {
            Selector::Slice(None, None, 1)
        } else {
            let start = self.integer()?;
            if self.take(':') {
                let end = self.integer()?;
                let step = if self.take(':') {
                    self.integer()?.unwrap_or(1)
                } else {
                    1
                };
                Selector::Slice(start, end, step)
            } else if let Some(start) = start {
                let mut indices = vec![start];
                while self.take(',') {
                    indices.push(
                        self.integer()?
                            .ok_or(RestMappingError::UnsupportedJsonPath)?,
                    );
                }
                Selector::Indices(indices)
            } else {
                Selector::Fields(self.fields()?)
            }
        };
        if !self.take(']') {
            return Err(RestMappingError::UnsupportedJsonPath);
        }
        Ok(selector)
    }
}

/// HTTP connector response conversion; 3xx is an error, not success or redirect.
pub fn decode_http_response(
    status: u16,
    content_type: &str,
    text: String,
) -> Result<RestResponse, AdapterFailure> {
    if !(200..300).contains(&status) {
        return Err(AdapterFailure::HttpStatus(status));
    }
    if content_type.contains("application/json") {
        serde_json::from_str(&text)
            .map(RestResponse::Json)
            .map_err(|_| AdapterFailure::Other)
    } else {
        Ok(RestResponse::HttpText(text))
    }
}
/// Transport errors are classified from reqwest's typed flags, not Display text.
pub fn classify_reqwest_error(error: &reqwest::Error, reading_body: bool) -> AdapterFailure {
    if let Some(status) = error.status() {
        AdapterFailure::HttpStatus(status.as_u16())
    } else if error.is_builder() || (error.is_connect() && error.is_timeout()) {
        // Python ConnectTimeout is neither a replayable ConnectError nor an
        // ambiguous read/write failure. The dispatcher has no dedicated variant.
        AdapterFailure::Other
    } else if error.is_timeout() {
        if reading_body {
            AdapterFailure::ReadTimeout
        } else {
            AdapterFailure::OperationTimeout
        }
    } else if error.is_connect() {
        AdapterFailure::ConnectError
    } else if reading_body {
        // Response::bytes labels transport-body failures as Decode, too. Do not
        // mistake that flag for an application JSON decoder error or lose the
        // dispatcher's mutation-outcome-unknown classification.
        AdapterFailure::ReadError
    } else if error.is_decode() {
        AdapterFailure::Other
    } else {
        AdapterFailure::WriteError
    }
}

pub struct ReqwestRestTransport {
    client: std::sync::RwLock<Option<Client>>,
    warmed_up: tokio::sync::Mutex<bool>,
    base_url: String,
    service: ServiceHandle,
    semaphore: Option<Arc<Semaphore>>,
    pool_timeout: Option<Duration>,
}
fn seconds(
    config: &Map<String, Value>,
    key: &'static str,
    default: f64,
) -> Result<Option<Duration>, RestMappingError> {
    match config.get(key) {
        Some(Value::Null) => Ok(None),
        value => {
            let number = value
                .map(|v| {
                    v.as_f64()
                        .ok_or(RestMappingError::InvalidConfig("invalid_http_timeout"))
                })
                .transpose()?
                .unwrap_or(default);
            Duration::try_from_secs_f64(number)
                .map(Some)
                .map_err(|_| RestMappingError::InvalidConfig("invalid_http_timeout"))
        }
    }
}
fn limit(
    config: &Map<String, Value>,
    key: &'static str,
    default: usize,
) -> Result<Option<usize>, RestMappingError> {
    match config.get(key) {
        Some(Value::Null) => Ok(None),
        value => value
            .map(|v| {
                v.as_u64().and_then(|v| usize::try_from(v).ok()).ok_or(
                    RestMappingError::InvalidConfig("invalid_http_connection_limit"),
                )
            })
            .transpose()
            .map(|v| Some(v.unwrap_or(default))),
    }
}
impl ReqwestRestTransport {
    pub fn new(service: &ServiceDefinition) -> Result<Self, RestMappingError> {
        Self::new_handle(&ServiceHandle::new(service.clone()))
    }
    pub fn new_handle(service: &ServiceHandle) -> Result<Self, RestMappingError> {
        let captured_service = service.clone();
        let service = service.snapshot();
        let empty = Map::new();
        let config = if python_truthy(&service.connector_config) {
            service
                .connector_config
                .as_object()
                .ok_or(RestMappingError::InvalidConfig("invalid_connector_config"))?
        } else {
            &empty
        };
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never());
        // Match httpx(http2=True): negotiate HTTP/2 while retaining HTTP/1
        // fallback; do not force HTTP/2 prior knowledge on every connection.
        if !config.get("http2").is_some_and(python_truthy) {
            builder = builder.http1_only();
        }
        if let Some(timeout) = seconds(config, "connect_timeout", 3.0)? {
            builder = builder.connect_timeout(timeout);
        }
        if let Some(timeout) = seconds(config, "stream_timeout", 300.0)? {
            builder = builder.read_timeout(timeout);
        }
        builder = builder.pool_idle_timeout(seconds(config, "keepalive_expiry", 120.0)?);
        if let Some(limit) = limit(config, "keepalive_connections", 50)? {
            builder = builder.pool_max_idle_per_host(limit);
        }
        let mut headers = HeaderMap::new();
        if let Some(value) = config.get("headers").filter(|v| python_truthy(v)) {
            let values = value
                .as_object()
                .ok_or(RestMappingError::InvalidConfig("invalid_http_headers"))?;
            for (name, value) in values {
                let name = HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| RestMappingError::InvalidConfig("invalid_http_header"))?;
                let mut value = HeaderValue::from_str(
                    value
                        .as_str()
                        .ok_or(RestMappingError::InvalidConfig("invalid_http_header"))?,
                )
                .map_err(|_| RestMappingError::InvalidConfig("invalid_http_header"))?;
                value.set_sensitive(true);
                headers.insert(name, value);
            }
        }
        let client = builder
            .default_headers(headers)
            .build()
            .map_err(|_| RestMappingError::InvalidConfig("http_client_construction_failed"))?;
        let base_url = config
            .get("base_url")
            .map(|v| {
                v.as_str()
                    .ok_or(RestMappingError::InvalidConfig("invalid_http_base_url"))
            })
            .transpose()?
            .unwrap_or("")
            .trim_end_matches('/')
            .to_owned();
        let _ = config
            .get("health_endpoint")
            .map(|v| {
                v.as_str()
                    .ok_or(RestMappingError::InvalidConfig("invalid_health_endpoint"))
            })
            .transpose()?;
        let semaphore = limit(config, "max_connections", 200)?
            .map(|n| {
                if n > Semaphore::MAX_PERMITS {
                    return Err(RestMappingError::InvalidConfig(
                        "invalid_http_connection_limit",
                    ));
                }
                Ok(Arc::new(Semaphore::new(n)))
            })
            .transpose()?;
        Ok(Self {
            client: std::sync::RwLock::new(Some(client)),
            warmed_up: tokio::sync::Mutex::new(false),
            base_url,
            service: captured_service,
            semaphore,
            pool_timeout: seconds(config, "pool_timeout", 10.0)?,
        })
    }
    fn client(&self) -> Result<Client, AdapterFailure> {
        self.client
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or(AdapterFailure::Other)
    }
    /// Best-effort bounded HEAD warm-up on the same reusable production client.
    pub async fn warm_up_connections(&self, count: usize) {
        let mut warmed = self.warmed_up.lock().await;
        if *warmed {
            return;
        }
        if let (Ok(client), Ok(url)) = (self.client(), self.resolve_url("/")) {
            futures_util::future::join_all((0..count.min(10)).map(|_| {
                let client = client.clone();
                let url = url.clone();
                async move {
                    let _ = client
                        .head(url)
                        .timeout(Duration::from_secs(2))
                        .send()
                        .await;
                }
            }))
            .await;
        }
        *warmed = true;
    }
    /// Stop admitting requests and release the idle pool. Already-owned calls
    /// retain their client until completion/cancellation, without a detached task.
    pub fn close(&self) {
        if let Some(semaphore) = &self.semaphore {
            semaphore.close();
        }
        self.client
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }
    /// httpx appends relative paths even when they begin with '/' and the base
    /// contains a path prefix. Standard URL join alone would silently erase it.
    pub fn resolve_url(&self, endpoint: &str) -> Result<Url, RestMappingError> {
        if endpoint.starts_with("//") {
            return Err(RestMappingError::InvalidConfig(
                "unsupported_scheme_relative_endpoint",
            ));
        }
        let url = if endpoint.contains("://") {
            Url::parse(endpoint)
        } else {
            Url::parse(&format!("{}/", self.base_url))
                .and_then(|base| base.join(endpoint.trim_start_matches('/')))
        }
        .map_err(|_| RestMappingError::InvalidConfig("invalid_http_endpoint"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(RestMappingError::InvalidConfig(
                "unsupported_http_endpoint_scheme",
            ));
        }
        Ok(url)
    }
    async fn acquire(&self) -> Result<Option<tokio::sync::OwnedSemaphorePermit>, AdapterFailure> {
        let Some(semaphore) = &self.semaphore else {
            return Ok(None);
        };
        let acquire = semaphore.clone().acquire_owned();
        let permit = if let Some(timeout) = self.pool_timeout {
            tokio::time::timeout(timeout, acquire)
                .await
                .map_err(|_| AdapterFailure::PoolTimeout)?
        } else {
            acquire.await
        };
        permit.map(Some).map_err(|_| AdapterFailure::Other)
    }
}
impl RestTransport for ReqwestRestTransport {
    fn shutdown(&self) -> RestTransportFuture<'_, ()> {
        Box::pin(async {
            self.close();
            Ok(())
        })
    }
    fn request(&self, request: RestRequest) -> RestTransportFuture<'_, RestResponse> {
        Box::pin(async move {
            let url = self
                .resolve_url(&request.endpoint)
                .map_err(|_| AdapterFailure::Other)?;
            let method = Method::from_bytes(request.method.to_uppercase().as_bytes())
                .map_err(|_| AdapterFailure::Other)?;
            // Build before acquiring/sending so invalid config cannot consume I/O.
            let client = self.client()?;
            let request = client
                .request(method, url)
                .json(&request.body)
                .build()
                .map_err(|e| classify_reqwest_error(&e, false))?;
            let _permit = self.acquire().await?;
            let response = client
                .execute(request)
                .await
                .map_err(|e| classify_reqwest_error(&e, false))?;
            let status = response.status().as_u16();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned();
            // httpx buffers the body before raise_for_status, so a body failure
            // takes precedence over a received error status.
            let bytes = response
                .bytes()
                .await
                .map_err(|e| classify_reqwest_error(&e, true))?;
            if !(200..300).contains(&status) {
                return Err(AdapterFailure::HttpStatus(status));
            }
            decode_http_response_bytes(status, &content_type, &bytes)
        })
    }
    fn health_check(&self) -> RestTransportFuture<'_, bool> {
        Box::pin(async {
            let config = self.service.connector().snapshot();
            let endpoint = match config.get("health_endpoint") {
                None => "/health",
                Some(Value::String(endpoint)) => endpoint,
                _ => return Ok(false),
            };
            let Ok(url) = self.resolve_url(endpoint) else {
                return Ok(false);
            };
            let Ok(_permit) = self.acquire().await else {
                return Ok(false);
            };
            let Ok(client) = self.client() else {
                return Ok(false);
            };
            let Ok(response) = client.get(url).send().await else {
                return Ok(false);
            };
            let healthy = response.status().as_u16() < 500;
            Ok(response.bytes().await.is_ok() && healthy)
        })
    }
}

#[path = "generic_rest_response.rs"]
mod response_bytes;
pub use response_bytes::decode_http_response_bytes;
