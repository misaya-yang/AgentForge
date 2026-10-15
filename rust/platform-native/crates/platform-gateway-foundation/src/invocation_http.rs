//! Public invoke/submit/stream route factories from `src/api/v1/`.
//!
//! No listener, provider adapter, task worker, or alternate execution loop is
//! created here. JWT authentication reuses the current account/role authority
//! and revocation checks used by the existing gateway. Other resolver modes
//! (API keys, guest cookies, disabled authentication) remain explicitly pending.
//! The hosting application owns global body limits and tracing middleware.
//! SSE is pull-driven: dropping the body cancels admission and drops its owned
//! GatewayStream, whose existing Drop path owns producer/history/usage cleanup.
use crate::native_contract::invocation_tasks as contract;
use crate::{
    capacity_admission::AdmissionCancellation,
    dispatch_preflight::{PreflightAuthority, PreflightError},
    dispatcher::DispatchError,
    gateway_dispatcher::{GatewayDispatchError, GatewayDispatcher, GatewayStream},
    protocol::{
        ContentData, ContentItem, ContentType, ProtocolTimestamp, StreamChunk, UnifiedRequest,
        UnifiedResponse,
    },
    request_validation::RequestValidationError,
    service_registry::RegistryError,
    session_manager::SessionError,
    task_manager::TaskError,
};
use ai_platform_auth::{
    api_keys::{SelectedCredential, select_credential},
    service::{AuthService, Principal},
};
use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use futures_core::Stream;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};
use std::{
    convert::Infallible,
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

#[derive(Clone)]
struct InvocationState {
    auth: Arc<AuthService>,
    dispatcher: Arc<GatewayDispatcher>,
}

/// The caller must use socket ConnectInfo (when available); forwarded headers
/// are intentionally ignored, matching these routes' `request.client.host`.
/// Merging this factory does not start a listener or a task consumer.
pub fn router(auth: Arc<AuthService>, dispatcher: Arc<GatewayDispatcher>) -> Router {
    Router::new()
        .route(contract::INVOKE, post(invoke))
        .route(contract::SUBMIT, post(submit))
        .route(contract::STREAM, post(stream))
        .with_state(InvocationState { auth, dispatcher })
}

/// Public JSON inputs deliberately reject structured `data`. The internal
/// dataclass permits it, but Pydantic ContentItemSchema accepts str/bytes/null;
/// JSON cannot represent bytes. Extra fields are accepted then dropped by
/// to_domain(), exactly like the source schema conversion.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ContentItemSchema {
    #[serde(rename = "type")]
    pub content_type: ContentType,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub metadata: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
impl ContentItemSchema {
    pub fn to_domain(self) -> ContentItem {
        ContentItem {
            content_type: self.content_type,
            data: self.data.map(ContentData::Text),
            url: self.url,
            mime_type: self.mime_type,
            metadata: self.metadata,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct UnifiedRequestSchema {
    pub request_id: Option<String>,
    pub service_id: String,
    pub inputs: Vec<ContentItemSchema>,
    pub session_id: Option<String>,
    pub context: Option<Map<String, Value>>,
    pub parameters: Option<Map<String, Value>>,
    pub callback_url: Option<String>,
    pub priority: i64,
    pub user_id: Option<String>,
    pub tenant_id: Option<String>,
    pub timestamp: Option<ProtocolTimestamp>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
impl UnifiedRequestSchema {
    pub fn to_domain(self, default_user_id: &str, default_tenant_id: &str) -> UnifiedRequest {
        UnifiedRequest {
            request_id: self
                .request_id
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| format!("req_{}", uuid::Uuid::new_v4().simple())),
            service_id: self.service_id,
            inputs: self
                .inputs
                .into_iter()
                .map(ContentItemSchema::to_domain)
                .collect(),
            session_id: self.session_id,
            context: self.context,
            parameters: self.parameters,
            callback_url: self.callback_url,
            priority: self.priority,
            user_id: default_user_id.into(),
            tenant_id: default_tenant_id.into(),
            timestamp: self.timestamp.unwrap_or_else(ProtocolTimestamp::now),
        }
    }

    /// Collect field errors rather than accepting the looser internal protocol.
    /// Integer coercion follows Pydantic's finite, integral JSON/string behavior.
    pub fn from_value(value: Value) -> Result<Self, RequestSchemaError> {
        let mut errors = Vec::new();
        let Some(object) = value.as_object() else {
            return Err(RequestSchemaError(vec![issue(
                "model_attributes_type",
                vec![json!("body")],
                "Input should be a valid dictionary or object to extract fields from",
                value,
            )]));
        };
        let mut normalized = object.clone();
        for name in [
            "request_id",
            "session_id",
            "callback_url",
            "user_id",
            "tenant_id",
        ] {
            validate_string(object, name, false, &mut errors);
        }
        validate_string(object, "service_id", true, &mut errors);
        for name in ["context", "parameters"] {
            if let Some(input) = object
                .get(name)
                .filter(|input| !input.is_null() && !input.is_object())
            {
                errors.push(issue(
                    "dict_type",
                    vec![json!("body"), json!(name)],
                    "Input should be a valid dictionary",
                    input.clone(),
                ));
            }
        }
        match object.get("inputs") {
            None => errors.push(issue(
                "missing",
                vec![json!("body"), json!("inputs")],
                "Field required",
                value.clone(),
            )),
            Some(Value::Array(inputs)) => {
                for (index, input) in inputs.iter().enumerate() {
                    validate_content(input, index, &mut errors);
                }
            }
            Some(input) => errors.push(issue(
                "list_type",
                vec![json!("body"), json!("inputs")],
                "Input should be a valid list",
                input.clone(),
            )),
        }
        let priority = match object.get("priority") {
            None => 0,
            Some(input) => match coerce_priority(input) {
                Ok(priority) => priority,
                Err((kind, message)) => {
                    errors.push(issue(
                        kind,
                        vec![json!("body"), json!("priority")],
                        message,
                        input.clone(),
                    ));
                    0
                }
            },
        };
        let timestamp = match object.get("timestamp").filter(|input| !input.is_null()) {
            None => None,
            Some(input) => match parse_timestamp(input) {
                Ok(timestamp) => Some(timestamp),
                Err((kind, explanation)) => {
                    let message = match kind {
                        "datetime_from_date_parsing" => {
                            format!("Input should be a valid datetime or date, {explanation}")
                        }
                        "datetime_parsing" => {
                            format!("Input should be a valid datetime, {explanation}")
                        }
                        _ => "Input should be a valid datetime".into(),
                    };
                    let mut error = issue(
                        kind,
                        vec![json!("body"), json!("timestamp")],
                        &message,
                        input.clone(),
                    );
                    if kind != "datetime_type" {
                        error["ctx"] = json!({"error":explanation});
                    }
                    errors.push(error);
                    None
                }
            },
        };
        if !errors.is_empty() {
            const FIELDS: [&str; 12] = [
                "request_id",
                "service_id",
                "inputs",
                "session_id",
                "context",
                "parameters",
                "callback_url",
                "priority",
                "user_id",
                "tenant_id",
                "timestamp",
                "",
            ];
            // Pydantic reports all fields in declaration order, with each nested
            // field's union errors kept in their original branch order.
            errors.sort_by_key(|error| {
                FIELDS
                    .iter()
                    .position(|field| Some(*field) == error["loc"][1].as_str())
                    .unwrap_or(FIELDS.len())
            });
            return Err(RequestSchemaError(errors));
        }
        let request_id = take_string(&mut normalized, "request_id");
        let service_id = take_string(&mut normalized, "service_id").expect("validated service ID");
        let inputs = serde_json::from_value(normalized.remove("inputs").expect("validated inputs"))
            .map_err(|_| {
                RequestSchemaError(vec![issue(
                    "model_attributes_type",
                    vec![json!("body"), json!("inputs")],
                    "Invalid content items",
                    Value::Null,
                )])
            })?;
        let result = Self {
            request_id,
            service_id,
            inputs,
            session_id: take_string(&mut normalized, "session_id"),
            context: take_object(&mut normalized, "context"),
            parameters: take_object(&mut normalized, "parameters"),
            callback_url: take_string(&mut normalized, "callback_url"),
            priority,
            user_id: take_string(&mut normalized, "user_id"),
            tenant_id: take_string(&mut normalized, "tenant_id"),
            timestamp,
            extra: {
                normalized.remove("priority");
                normalized.remove("timestamp");
                normalized
            },
        };
        Ok(result)
    }
}
impl<'de> Deserialize<'de> for UnifiedRequestSchema {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_value(Value::deserialize(deserializer)?)
            .map_err(|_| serde::de::Error::custom("invalid public request schema"))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RequestSchemaError(pub Vec<Value>);
impl IntoResponse for RequestSchemaError {
    fn into_response(self) -> Response {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"detail":self.0})),
        )
            .into_response()
    }
}
fn issue(kind: &str, loc: Vec<Value>, message: &str, input: Value) -> Value {
    json!({"type":kind,"loc":loc,"msg":message,"input":input})
}
fn validate_string(
    object: &Map<String, Value>,
    name: &str,
    required: bool,
    errors: &mut Vec<Value>,
) {
    match object.get(name) {
        None if required => errors.push(issue(
            "missing",
            vec![json!("body"), json!(name)],
            "Field required",
            Value::Object(object.clone()),
        )),
        Some(input) if !input.is_string() && (required || !input.is_null()) => errors.push(issue(
            "string_type",
            vec![json!("body"), json!(name)],
            "Input should be a valid string",
            input.clone(),
        )),
        _ => (),
    }
}
fn validate_content(value: &Value, index: usize, errors: &mut Vec<Value>) {
    let location = vec![json!("body"), json!("inputs"), json!(index)];
    let Some(object) = value.as_object() else {
        errors.push(issue(
            "model_attributes_type",
            location,
            "Input should be a valid dictionary or object to extract fields from",
            value.clone(),
        ));
        return;
    };
    let field = |name: &str| {
        let mut loc = location.clone();
        loc.push(json!(name));
        loc
    };
    match object.get("type") {
        None => errors.push(issue(
            "missing",
            field("type"),
            "Field required",
            value.clone(),
        )),
        Some(input) if serde_json::from_value::<ContentType>(input.clone()).is_err() => {
            let expected =
                "'text', 'image', 'audio', 'video', 'file', 'json', 'tool_call' or 'tool_result'";
            let mut error = issue(
                "enum",
                field("type"),
                &format!("Input should be {expected}"),
                input.clone(),
            );
            error["ctx"] = json!({"expected":expected});
            errors.push(error);
        }
        _ => (),
    }
    for name in ["data", "url", "mime_type"] {
        if let Some(input) = object
            .get(name)
            .filter(|input| !input.is_null() && !input.is_string())
        {
            if name == "data" {
                for (branch, kind, message) in [
                    ("str", "string_type", "Input should be a valid string"),
                    ("bytes", "bytes_type", "Input should be a valid bytes"),
                ] {
                    let mut loc = field(name);
                    loc.push(json!(branch));
                    errors.push(issue(kind, loc, message, input.clone()));
                }
            } else {
                errors.push(issue(
                    "string_type",
                    field(name),
                    "Input should be a valid string",
                    input.clone(),
                ));
            }
        }
    }
    if let Some(input) = object
        .get("metadata")
        .filter(|input| !input.is_null() && !input.is_object())
    {
        errors.push(issue(
            "dict_type",
            field("metadata"),
            "Input should be a valid dictionary",
            input.clone(),
        ));
    }
}
fn take_string(object: &mut Map<String, Value>, name: &str) -> Option<String> {
    object
        .remove(name)
        .and_then(|value| value.as_str().map(str::to_owned))
}
fn take_object(object: &mut Map<String, Value>, name: &str) -> Option<Map<String, Value>> {
    object
        .remove(name)
        .and_then(|value| value.as_object().cloned())
}
fn coerce_priority(value: &Value) -> Result<i64, (&'static str, &'static str)> {
    match value {
        Value::Bool(value) => Ok(i64::from(*value)),
        Value::Number(number) if number.as_i64().is_some() => Ok(number.as_i64().expect("checked")),
        Value::Number(number) => {
            let number = number.as_f64().unwrap_or(f64::NAN);
            if number.fract() != 0.0 {
                return Err((
                    "int_from_float",
                    "Input should be a valid integer, got a number with a fractional part",
                ));
            }
            if number >= i64::MIN as f64 && number < -(i64::MIN as f64) {
                Ok(number as i64)
            } else {
                Err((
                    "int_parsing_size",
                    "Unable to parse input string as an integer, exceeded maximum size",
                ))
            }
        }
        Value::String(text) => {
            let text = text.trim();
            let bytes = text.as_bytes();
            if bytes.iter().enumerate().any(|(index, byte)| {
                *byte == b'_'
                    && (index == 0
                        || !bytes[index - 1].is_ascii_digit()
                        || !bytes.get(index + 1).is_some_and(u8::is_ascii_digit))
            }) {
                return Err((
                    "int_parsing",
                    "Input should be a valid integer, unable to parse string as an integer",
                ));
            }
            if text
                .split_once('.')
                .is_some_and(|(_, fraction)| fraction.contains('_'))
            {
                return Err((
                    "int_parsing",
                    "Input should be a valid integer, unable to parse string as an integer",
                ));
            }
            let cleaned = text.replace('_', "");
            let integer = match cleaned.split_once('.') {
                Some((integer, fraction))
                    if !fraction.is_empty() && fraction.bytes().all(|byte| byte == b'0') =>
                {
                    integer
                }
                None => &cleaned,
                _ => {
                    return Err((
                        "int_parsing",
                        "Input should be a valid integer, unable to parse string as an integer",
                    ));
                }
            };
            integer.parse().map_err(|_| {
                (
                    "int_parsing",
                    "Input should be a valid integer, unable to parse string as an integer",
                )
            })
        }
        _ => Err(("int_type", "Input should be a valid integer")),
    }
}
type TimestampError = (&'static str, &'static str);
fn parse_timestamp(value: &Value) -> Result<ProtocolTimestamp, TimestampError> {
    if let Value::String(text) = value {
        // Speedate accepts decimal epoch strings, but not exponents, whitespace,
        // hour-only ISO strings, leap seconds or second-resolution UTC offsets.
        let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
        let mut decimal = unsigned.split('.');
        let whole = decimal.next().unwrap_or("");
        let fraction = decimal.next();
        let numeric = !whole.is_empty()
            && whole.bytes().all(|byte| byte.is_ascii_digit())
            && fraction.is_none_or(|value| {
                !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
            })
            && decimal.next().is_none();
        if numeric && let Ok(number) = text.parse::<f64>() {
            return epoch_timestamp(number);
        }
        let bytes = text.as_bytes();
        let date_error = |message| Err(("datetime_from_date_parsing", message));
        if bytes.len() < 10 {
            return date_error("input is too short");
        }
        if !bytes[..4].iter().all(u8::is_ascii_digit) {
            return date_error("invalid character in year");
        }
        if bytes[4] != b'-' || bytes[7] != b'-' {
            return date_error("invalid date separator, expected `-`");
        }
        if !bytes[5..7].iter().all(u8::is_ascii_digit) {
            return date_error("invalid character in month");
        }
        if !bytes[8..10].iter().all(u8::is_ascii_digit) {
            return date_error("invalid character in day");
        }
        let year = text[..4].parse::<i32>().expect("ASCII year");
        let month = text[5..7].parse::<u32>().expect("ASCII month");
        let day = text[8..10].parse::<u32>().expect("ASCII day");
        if !(1..=12).contains(&month) {
            return date_error("month value is outside expected range of 1-12");
        }
        let Some(date) = chrono::NaiveDate::from_ymd_opt(year, month, day) else {
            return date_error("day value is outside expected range");
        };
        if year == 0 {
            return Err(("datetime_parsing", "year 0 is out of range"));
        }
        if bytes.len() == 10 {
            return Ok(date.and_hms_opt(0, 0, 0).expect("midnight").into());
        }
        let invalid_time = || {
            (
                "datetime_from_date_parsing",
                "unexpected extra characters at the end of the input",
            )
        };
        if bytes.len() < 16 || !matches!(bytes[10], b'T' | b't' | b' ' | b'_') || bytes[13] != b':'
        {
            return Err(invalid_time());
        }
        let digits = |start: usize| -> Option<u32> {
            let data = bytes.get(start..start + 2)?;
            if data.iter().all(u8::is_ascii_digit) {
                Some(u32::from(data[0] - b'0') * 10 + u32::from(data[1] - b'0'))
            } else {
                None
            }
        };
        let hour = digits(11).ok_or_else(invalid_time)?;
        let minute = digits(14).ok_or_else(invalid_time)?;
        let mut position = 16;
        let mut second = 0;
        if bytes.get(position) == Some(&b':') {
            second = digits(position + 1).ok_or_else(invalid_time)?;
            position += 3;
        }
        let mut micros = 0;
        if matches!(bytes.get(position), Some(b'.' | b',')) {
            position += 1;
            let start = position;
            while bytes.get(position).is_some_and(u8::is_ascii_digit) {
                position += 1;
            }
            if start == position {
                return Err(invalid_time());
            }
            for index in 0..6 {
                micros = micros * 10
                    + bytes
                        .get(start + index)
                        .filter(|_| start + index < position)
                        .map_or(0, |byte| u32::from(byte - b'0'));
            }
        }
        if second > 59 {
            return Err(invalid_time());
        }
        let time = date
            .and_hms_micro_opt(hour, minute, second, micros)
            .ok_or_else(invalid_time)?;
        if position == bytes.len() {
            return Ok(time.into());
        }
        let offset = match bytes[position] {
            b'Z' | b'z' if position + 1 == bytes.len() => 0,
            b'+' | b'-' => {
                let sign = if bytes[position] == b'-' { -1 } else { 1 };
                let hours = digits(position + 1).ok_or_else(invalid_time)?;
                let minutes =
                    if bytes.get(position + 3) == Some(&b':') && position + 6 == bytes.len() {
                        digits(position + 4)
                    } else if position + 5 == bytes.len() {
                        digits(position + 3)
                    } else {
                        None
                    }
                    .ok_or_else(invalid_time)?;
                if hours > 23 || minutes > 59 {
                    return Err(invalid_time());
                }
                sign * ((hours * 3600 + minutes * 60) as i32)
            }
            _ => return Err(invalid_time()),
        };
        let offset = chrono::FixedOffset::east_opt(offset).ok_or_else(invalid_time)?;
        return Ok(time
            .and_local_timezone(offset)
            .single()
            .ok_or_else(invalid_time)?
            .into());
    }
    match value.as_f64() {
        Some(number) if value.is_number() => epoch_timestamp(number),
        _ => Err(("datetime_type", "")),
    }
}
fn epoch_timestamp(number: f64) -> Result<ProtocolTimestamp, TimestampError> {
    use chrono::Datelike;
    let range_error = if number < 0.0 {
        "dates before 0000 are not supported as unix timestamps"
    } else {
        "dates after 9999 are not supported as unix timestamps"
    };
    if !number.is_finite() {
        return Err(("datetime_parsing", range_error));
    }
    let seconds = if number.abs() > 20_000_000_000.0 {
        number / 1000.0
    } else {
        number
    };
    let whole = seconds.floor();
    let micros = ((seconds - whole) * 1_000_000.0).round() as u32;
    let (whole, micros) = if micros == 1_000_000 {
        (whole + 1.0, 0)
    } else {
        (whole, micros)
    };
    let time = chrono::DateTime::from_timestamp(whole as i64, micros * 1000)
        .ok_or(("datetime_parsing", range_error))?;
    if !(1..=9999).contains(&time.year()) {
        return Err(("datetime_parsing", range_error));
    }
    Ok(time.fixed_offset().into())
}

/// These wrappers preserve explicit null fields and bytes->base64 projections.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(transparent)]
pub struct UnifiedResponseSchema(Value);
impl UnifiedResponseSchema {
    pub fn from_domain(response: &UnifiedResponse) -> Self {
        let mut value = response.to_wire_value();
        // Pydantic JSON renders UTC offsets as Z; internal dataclasses use +00:00.
        if let Some(timestamp) = value["timestamp"]
            .as_str()
            .and_then(|time| time.strip_suffix("+00:00"))
        {
            value["timestamp"] = Value::String(format!("{timestamp}Z"));
        }
        Self(value)
    }
}
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(transparent)]
pub struct StreamChunkSchema(Value);
impl StreamChunkSchema {
    pub fn from_domain(chunk: &StreamChunk) -> Self {
        Self(chunk.to_wire_value())
    }
}

pub(crate) fn detail_error(status: StatusCode, detail: impl Into<String>) -> Response {
    (status, Json(json!({"detail":detail.into()}))).into_response()
}
pub(crate) fn internal_error(headers: &HeaderMap) -> Response {
    let mut value = json!({"error":{"code":2000,"message":"Internal server error"}});
    if let Some(trace_id) = headers
        .get("x-trace-id")
        .or_else(|| headers.get("x-request-id"))
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
    {
        value["error"]["trace_id"] = json!(trace_id);
    }
    (StatusCode::INTERNAL_SERVER_ERROR, Json(value)).into_response()
}
pub(crate) async fn authenticate(
    auth: &AuthService,
    headers: &HeaderMap,
) -> Result<Principal, Box<Response>> {
    let token = match select_credential(headers, &HeaderName::from_static("x-api-key")) {
        Ok(SelectedCredential::Jwt(token)) => token,
        _ => {
            return Err(detail_error(StatusCode::UNAUTHORIZED, "Authentication required").into());
        }
    };
    let mut principal = auth.authenticate(token).await.map_err(|error| {
        detail_error(
            StatusCode::from_u16(error.status()).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
            "Authentication failed",
        )
    })?;
    if principal.account.user_id.is_empty() || principal.account.tenant_id.is_empty() {
        return Err(detail_error(StatusCode::UNAUTHORIZED, "Authentication required").into());
    }
    // Canonical get_user_context and RequestPrincipal::jwt append current DB
    // permissions to the role subjects consumed by RequestValidator. Never use
    // the JWT's potentially stale role/permission claims or public body fields.
    principal.authority.roles =
        resolved_subjects(&principal.authority.roles, &principal.authority.permissions);
    Ok(principal)
}
fn resolved_subjects(roles: &[String], permissions: &[String]) -> Vec<String> {
    let mut subjects = Vec::with_capacity(roles.len() + permissions.len());
    for subject in roles.iter().chain(permissions) {
        if !subjects.contains(subject) {
            subjects.push(subject.clone());
        }
    }
    subjects
}

fn authority(principal: &Principal) -> PreflightAuthority<'_> {
    PreflightAuthority::from_session_principal(principal)
}

/// Preserve the legacy GatewayError handler, including AuthenticationRequired's
/// historical 400 (it is not the separate AuthError class). Non-GatewayError
/// failures use the generic sanitized 500; capacity fields are not fabricated
/// into route headers, because the Python invoke/submit routes do not do so.
pub fn gateway_error_response(error: &GatewayDispatchError, headers: &HeaderMap) -> Response {
    let status = match error {
        GatewayDispatchError::Task(TaskError::AdmissionUnconfirmed(task_id)) => {
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":"task_admission_unconfirmed","task_id":task_id}))).into_response();
        }
        GatewayDispatchError::Task(TaskError::DurableAuthorityRequired) => StatusCode::FORBIDDEN,
        GatewayDispatchError::Preflight(PreflightError::ServiceNotFound(_)) => {
            StatusCode::NOT_FOUND
        }
        GatewayDispatchError::Preflight(PreflightError::Validation(
            RequestValidationError::PermissionDenied(_),
        ))
        | GatewayDispatchError::Session(SessionError::PermissionDenied(_)) => StatusCode::FORBIDDEN,
        GatewayDispatchError::Preflight(PreflightError::RateLimited(_)) => {
            StatusCode::TOO_MANY_REQUESTS
        }
        GatewayDispatchError::Dispatch(DispatchError::CircuitOpen { service_id }) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":format!("Circuit breaker open for {service_id}")})),
            )
                .into_response();
        }
        GatewayDispatchError::Preflight(PreflightError::Validation(
            RequestValidationError::SchemaConfiguration(_),
        )) => return internal_error(headers),
        GatewayDispatchError::Dispatch(DispatchError::AdapterValidationFailed(_))
        | GatewayDispatchError::Dispatch(DispatchError::AdapterValidationDetail(_))
        | GatewayDispatchError::Preflight(PreflightError::Validation(_))
        | GatewayDispatchError::Registry(
            RegistryError::AdapterNotFound(_) | RegistryError::ValidationFailed(_),
        )
        | GatewayDispatchError::Preflight(PreflightError::Registry(
            RegistryError::AdapterNotFound(_) | RegistryError::ValidationFailed(_),
        ))
        | GatewayDispatchError::Session(SessionError::AlreadyExists)
        | GatewayDispatchError::Task(TaskError::NotFound(_)) => StatusCode::BAD_REQUEST,
        _ => return internal_error(headers),
    };
    (status, Json(json!({"error":error.to_string()}))).into_response()
}

async fn request_context(
    state: &InvocationState,
    request: Request,
) -> Result<(UnifiedRequest, Principal, Option<String>, HeaderMap), Box<Response>> {
    let (parts, body) = request.into_parts();
    let peer = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|peer| peer.0.ip().to_string());
    // Source has no route-local cap on these three endpoints. Hosting middleware
    // must choose deployment body budgets rather than inheriting Axum's 2 MiB.
    let bytes = to_bytes(body, usize::MAX).await.map_err(|_| {
        detail_error(
            StatusCode::BAD_REQUEST,
            "There was an error parsing the body",
        )
    })?;
    let is_json = crate::http_content_type::is_json_content_type(&parts.headers);
    let value = if bytes.is_empty() {
        Value::Null
    } else if is_json {
        serde_json::from_slice(&bytes).map_err(|error| {
            RequestSchemaError(vec![json!({"type":"json_invalid","loc":["body",json_error_location(&bytes,&error)],"msg":"JSON decode error","input":{},"ctx":{"error":error.to_string()}})]).into_response()
        })?
    } else {
        Value::String(String::from_utf8_lossy(&bytes).into_owned())
    };
    let principal = authenticate(&state.auth, &parts.headers).await?;
    if value.is_null() {
        return Err(RequestSchemaError(vec![issue(
            "missing",
            vec![json!("body")],
            "Field required",
            Value::Null,
        )])
        .into_response()
        .into());
    }
    let schema = UnifiedRequestSchema::from_value(value).map_err(IntoResponse::into_response)?;
    let request = schema.to_domain(&principal.account.user_id, &principal.account.tenant_id);
    Ok((request, principal, peer, parts.headers))
}
/// FastAPI exposes character offsets, whereas serde reports byte columns. EOF
/// points beyond the last byte, not at the final character. Parser-specific
/// diagnostics/locations (e.g. unterminated strings/escapes) remain a documented
/// migration boundary; this is intentionally not a second JSON parser.
fn json_error_location(bytes: &[u8], error: &serde_json::Error) -> usize {
    let offset = if error.is_eof() {
        bytes.len()
    } else {
        let before_line: usize = bytes
            .split(|byte| *byte == b'\n')
            .take(error.line().saturating_sub(1))
            .map(|line| line.len() + 1)
            .sum();
        before_line
            .saturating_add(error.column().saturating_sub(1))
            .min(bytes.len())
    };
    String::from_utf8_lossy(&bytes[..offset]).chars().count()
}

struct CancelOnDrop(AdmissionCancellation);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
async fn invoke(State(state): State<InvocationState>, request: Request) -> Response {
    let (request, principal, client_ip, headers) = match request_context(&state, request).await {
        Ok(context) => context,
        Err(response) => return *response,
    };
    let sdk_read_lease = match state.dispatcher.sdk_read_lease(&request,&principal,&headers).await {
        Ok(lease)=>lease, Err(error)=>return gateway_error_response(&error,&headers),
    };
    let cancellation = AdmissionCancellation::default();
    let _guard = CancelOnDrop(cancellation.clone());
    let _sdk_read_lease=sdk_read_lease;
    match state
        .dispatcher
        .invoke(
            request,
            &authority(&principal),
            client_ip.as_deref(),
            cancellation,
        )
        .await
    {
        Ok(response) => Json(UnifiedResponseSchema::from_domain(&response)).into_response(),
        Err(error) => gateway_error_response(&error, &headers),
    }
}
async fn submit(State(state): State<InvocationState>, request: Request) -> Response {
    let (request, principal, client_ip, headers) = match request_context(&state, request).await {
        Ok(context) => context,
        Err(response) => return *response,
    };
    let request_id = request.request_id.clone();
    match state
        .dispatcher
        .submit_with_session(request, &authority(&principal), client_ip.as_deref())
        .await
    {
        Ok((task_id, session_id)) => {
            let mut response = UnifiedResponse::new(request_id, "pending", Vec::new());
            response.task_id = Some(task_id);
            response.session_id = session_id;
            Json(UnifiedResponseSchema::from_domain(&response)).into_response()
        }
        Err(error) => gateway_error_response(&error, &headers),
    }
}
async fn stream(State(state): State<InvocationState>, request: Request) -> Response {
    let (mut request, principal, client_ip, headers) = match request_context(&state, request).await
    {
        Ok(context) => context,
        Err(response) => return *response,
    };
    let session_id = match state
        .dispatcher
        .prepare_stream_session(&mut request, &authority(&principal))
        .await
    {
        Ok(session) => session,
        Err(
            error @ GatewayDispatchError::Session(
                SessionError::AlreadyExists | SessionError::PermissionDenied(_),
            ),
        )
        | Err(
            error @ GatewayDispatchError::Registry(
                RegistryError::ValidationFailed(_) | RegistryError::AdapterNotFound(_),
            ),
        ) => return detail_error(StatusCode::FORBIDDEN, error.to_string()),
        Err(_) => return internal_error(&headers),
    };
    let session_header = match session_id
        .filter(|id| !id.is_empty())
        .map(|id| HeaderValue::from_str(&id))
        .transpose()
    {
        Ok(header) => header,
        Err(_) => return internal_error(&headers),
    };
    let sdk_read_lease = match state.dispatcher.sdk_read_lease(&request,&principal,&headers).await {
        Ok(lease)=>lease, Err(error)=>return gateway_error_response(&error,&headers),
    };
    let cancellation = AdmissionCancellation::default();
    let signal = cancellation.clone();
    // Opening occurs on first body poll, after the HTTP 200 headers. Python's
    // dispatcher is an async generator, so service/preflight/adapter errors must
    // also be sent as SSE error events rather than replacing the HTTP status.
    let opening = Box::pin(async move {
        state
            .dispatcher
            .stream(
                request,
                &authority(&principal),
                client_ip.as_deref(),
                signal,
            )
            .await
    });
    let mut response = Response::new(Body::from_stream(InvocationSse {
        state: SseState::Opening(opening),
        cancellation,
        sdk_read_lease,
    }));
    let headers = response.headers_mut();
    headers.insert(
        "content-type",
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    headers.insert("cache-control", HeaderValue::from_static("no-cache"));
    headers.insert("connection", HeaderValue::from_static("keep-alive"));
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    if let Some(session) = session_header {
        headers.insert("x-session-id", session);
    }
    response
}

type OpenFuture = Pin<Box<dyn Future<Output = Result<GatewayStream, GatewayDispatchError>> + Send>>;
type NextFuture = Pin<
    Box<
        dyn Future<
                Output = (
                    Box<GatewayStream>,
                    Result<Option<StreamChunk>, GatewayDispatchError>,
                ),
            > + Send,
    >,
>;
enum SseState {
    Opening(OpenFuture),
    Ready(Box<GatewayStream>),
    Reading(NextFuture),
    Done,
}
struct InvocationSse {
    sdk_read_lease:Option<crate::sdk_read_grants::SdkReadLease>,
    state: SseState,
    cancellation: AdmissionCancellation,
}
impl Drop for InvocationSse {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}
impl Stream for InvocationSse {
    type Item = Result<Bytes, Infallible>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match &mut this.state {
                SseState::Opening(future) => match future.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(stream)) => this.state = SseState::Ready(Box::new(stream)),
                    Poll::Ready(Err(error)) => {
                        this.state = SseState::Done;
                        return Poll::Ready(Some(Ok(error_event(&error))));
                    }
                },
                SseState::Ready(_) => {
                    let SseState::Ready(mut stream) =
                        std::mem::replace(&mut this.state, SseState::Done)
                    else {
                        unreachable!()
                    };
                    this.state = SseState::Reading(Box::pin(async move {
                        let chunk = stream.next().await;
                        (stream, chunk)
                    }));
                }
                SseState::Reading(future) => match future.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready((stream, Ok(Some(chunk)))) => {
                        if chunk.is_final {this.sdk_read_lease.take();}
                        this.state = SseState::Ready(stream);
                        let payload =
                            serde_json::to_string(&StreamChunkSchema::from_domain(&chunk))
                                .expect("protocol values serialize");
                        return Poll::Ready(Some(Ok(Bytes::from(format!("data: {payload}\n\n")))));
                    }
                    Poll::Ready((_stream, Ok(None))) => {
                        this.state = SseState::Done;
                        return Poll::Ready(None);
                    }
                    Poll::Ready((_stream, Err(error))) => {
                        this.state = SseState::Done;
                        return Poll::Ready(Some(Ok(error_event(&error))));
                    }
                },
                SseState::Done => {this.sdk_read_lease.take(); return Poll::Ready(None)},
            }
        }
    }
}
fn error_event(error: &GatewayDispatchError) -> Bytes {
    let message = match error {
        GatewayDispatchError::Dispatch(DispatchError::CircuitOpen { service_id }) => {
            format!("Circuit breaker open for {service_id}")
        }
        _ => error.to_string(),
    };
    Bytes::from(format!("event: error\ndata: {message}\n\n"))
}

