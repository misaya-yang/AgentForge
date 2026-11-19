//! Inert, opt-in fail-closed direct-upload contract from `src/api/v1/presign.py`.
//! No storage, signing, queue, document ACL, tenant gate or capability gate exists.
//! The host supplies canonical get_user_context semantics, including anonymous
//! values and dependency errors. Do not substitute an admission-required check:
//! syntactically valid anonymous bad models must return 422 before endpoint 401.
//! Syntax errors precede resolution; model errors follow it. Trusted credentials,
//! global middleware, body limits, redirects and exotic JSON encoding/nonfinite
//! numeric handling remain host boundaries, as for the existing route libraries.
use crate::{
    config_audit_writer::ConfigAuditRequestContext,
    service_config_api::{ServiceConfigCancellation, ServiceConfigError, ServiceConfigFuture},
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{FromRequest, MatchedPath, Path, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Value, json, value::RawValue};
use std::{collections::HashMap, sync::Arc};

pub const DIRECT_UPLOAD_UNAVAILABLE: &str =
    "Direct presigned upload is not implemented; use the standard upload API.";
pub const PRESIGN_ROUTES: &[(&str, &str, &str)] = &[
    ("POST", "/presign/upload", "get_presigned_upload_url"),
    ("POST", "/presign/confirm", "confirm_upload"),
    ("GET", "/presign/status/{task_id}", "get_task_status"),
];
/// A trusted get_user_context result, never constructed from body/query headers.
/// Tenant, roles and user ID are deliberately absent: endpoints do not read them.
#[derive(Clone, Default)]
pub struct PresignIdentity {
    pub is_authenticated: bool,
}
pub trait PresignIdentityResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> ServiceConfigFuture<'a, PresignIdentity>;
}
#[derive(Clone, Copy)]
pub enum PresignOperation {
    Upload,
    Confirm,
    Status,
}
impl PresignOperation {
    fn body(self) -> bool {
        !matches!(self, Self::Status)
    }
}
pub struct PresignApi {
    pub identity: Arc<dyn PresignIdentityResolver>,
}
impl PresignApi {
    pub fn new(identity: Arc<dyn PresignIdentityResolver>) -> Self {
        Self { identity }
    }
    pub fn execute(
        &self,
        operation: PresignOperation,
        identity: &PresignIdentity,
        body: &Value,
    ) -> Result<Value, ServiceConfigError> {
        if operation.body() {
            validate_body(operation, body)?;
        }
        Err(ServiceConfigError::detail(
            if identity.is_authenticated { 501 } else { 401 },
            if identity.is_authenticated {
                DIRECT_UPLOAD_UNAVAILABLE
            } else {
                "Authentication required"
            },
        ))
    }
}
fn issue(kind: &str, field: Option<&str>, input: &Value) -> Value {
    let msg = match kind {
        "missing" => "Field required",
        "string_type" => "Input should be a valid string",
        "dict_type" => "Input should be a valid dictionary",
        "model_attributes_type" => {
            "Input should be a valid dictionary or object to extract fields from"
        }
        "int_type" => "Input should be a valid integer",
        "int_parsing" => "Input should be a valid integer, unable to parse string as an integer",
        "int_from_float" => "Input should be a valid integer, got a number with a fractional part",
        "int_parsing_size" => "Unable to parse input string as an integer, exceeded maximum size",
        _ => unreachable!(),
    };
    let loc = field.map_or(json!(["body"]), |v| json!(["body", v]));
    json!({"type":kind,"loc":loc,"msg":msg,"input":input})
}
/// Acceptance only: Python integers have no i64 bound and the endpoint never
/// consumes the result. Reuse shared integer coercion first, then admit valid
/// arbitrary-precision decimal strings that overflow that helper.
fn integer_kind(value: &Value) -> Option<&'static str> {
    if value.is_null() {
        return None;
    }
    // Integer values within Value's representation are acceptance-only. The
    // route retains larger JSON integer lexemes separately with RawValue.
    if let Value::Number(n) = value {
        let text = n.to_string();
        if !text.contains(['.', 'e', 'E']) {
            return None;
        }
    }
    match crate::rate_config_api::integer(value) {
        Ok(_) => None,
        Err("int_parsing_size") => match value {
            Value::Number(_) => None,
            Value::String(s) => {
                let s = s.trim();
                let s = s.strip_prefix(['+', '-']).unwrap_or(s);
                let (whole, fraction) = s.split_once('.').map_or((s, None), |(a, b)| (a, Some(b)));
                let b = whole.as_bytes();
                if !b.is_empty()
                    && b.iter().enumerate().all(|(i, c)| {
                        c.is_ascii_digit()
                            || (*c == b'_'
                                && i > 0
                                && b[i - 1].is_ascii_digit()
                                && b.get(i + 1).is_some_and(u8::is_ascii_digit))
                    })
                    && fraction.is_none_or(|s| !s.is_empty() && s.bytes().all(|b| b == b'0'))
                {
                    let digits = whole.replace('_', "");
                    if digits.trim_start_matches('0').len() > 4300 {
                        Some(if whole.contains('_') {
                            "int_parsing"
                        } else {
                            "int_parsing_size"
                        })
                    } else {
                        None
                    }
                } else {
                    Some("int_parsing")
                }
            }
            _ => Some("int_type"),
        },
        Err(kind) => Some(kind),
    }
}
pub fn validate_body(operation: PresignOperation, body: &Value) -> Result<(), ServiceConfigError> {
    let Some(fields) = body.as_object() else {
        return Err(ServiceConfigError::validation(vec![issue(
            if body.is_null() {
                "missing"
            } else {
                "model_attributes_type"
            },
            None,
            body,
        )]));
    };
    let strings: &[&str] = match operation {
        PresignOperation::Upload => &["filename", "content_type", "document_id"],
        PresignOperation::Confirm => &[
            "upload_id",
            "storage_key",
            "document_id",
            "filename",
            "content_type",
        ],
        PresignOperation::Status => return Ok(()),
    };
    let mut errors = Vec::new();
    for field in strings {
        match fields.get(*field) {
            None => errors.push(issue("missing",Some(field),body)),
            Some(Value::String(s)) if matches!(operation,PresignOperation::Upload) && *field=="filename" && s.chars().count()>255 => errors.push(json!({"type":"string_too_long","loc":["body",field],"msg":"String should have at most 255 characters","input":s,"ctx":{"max_length":255}})),
            Some(Value::String(_)) => (),
            Some(v) => errors.push(issue("string_type",Some(field),v)),
        }
    }
    if let Some(v) = fields.get("file_size_bytes")
        && let Some(kind) = integer_kind(v)
    {
        errors.push(issue(kind, Some("file_size_bytes"), v));
    }
    if matches!(operation, PresignOperation::Upload)
        && let Some(v) = fields.get("metadata")
        && !v.is_null()
        && !v.is_object()
    {
        errors.push(issue("dict_type", Some("metadata"), v));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ServiceConfigError::validation(errors))
    }
}
#[derive(Clone)]
struct HttpState {
    api: Arc<PresignApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
}
pub fn router(api: Arc<PresignApi>, cancellation: Arc<dyn ServiceConfigCancellation>) -> Router {
    Router::new()
        .route("/api/v1/presign/upload", any(dispatch))
        .route("/api/v1/presign/confirm", any(dispatch))
        .route("/api/v1/presign/status/{task_id}", any(dispatch))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))) })
        .with_state(HttpState { api, cancellation })
}
async fn respond(
    state: &HttpState,
    result: Result<Value, ServiceConfigError>,
    headers: &HeaderMap,
) -> Response {
    match result {
        Ok(v) => Json(v).into_response(),
        Err(ServiceConfigError::Cancelled) => state.cancellation.cancelled().await,
        Err(e) => {
            let (status, body) = e
                .response(&ConfigAuditRequestContext {
                    headers: headers.clone(),
                    ..Default::default()
                })
                .expect("not cancelled");
            (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(body),
            )
                .into_response()
        }
    }
}
// Translate the common serde syntax categories to CPython's public JSON error
// text and character offsets; exotic encodings remain a trusted host boundary.
fn python_string_error(text: &str) -> Option<(usize, &'static str)> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        let opening = i;
        i += 1;
        loop {
            let Some(&byte) = bytes.get(i) else {
                return Some((opening, "Unterminated string starting at"));
            };
            if byte == b'"' {
                i += 1;
                break;
            }
            if byte < 0x20 {
                return Some((i, "Invalid control character at"));
            }
            if byte == b'\\' {
                let slash = i;
                i += 1;
                match bytes.get(i) {
                    Some(b'u') => {
                        if bytes
                            .get(i + 1..i + 5)
                            .is_none_or(|hex| !hex.iter().all(u8::is_ascii_hexdigit))
                        {
                            return Some((i, "Invalid \\uXXXX escape"));
                        }
                        i += 5;
                    }
                    Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => i += 1,
                    Some(_) => return Some((slash, "Invalid \\escape")),
                    None => return Some((opening, "Unterminated string starting at")),
                }
            } else {
                i += 1;
            }
        }
    }
    None
}
fn json_error(bytes: &[u8], error: serde_json::Error) -> ServiceConfigError {
    let text = String::from_utf8_lossy(bytes);
    let message = error.to_string();
    let byte_offset = text
        .split_inclusive('\n')
        .take(error.line().saturating_sub(1))
        .map(str::len)
        .sum::<usize>()
        + error.column();
    let mut offset = byte_offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let mut open_quote = None;
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        if i >= offset {
            break;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if open_quote.is_some() && c == '\\' {
            escaped = true;
            continue;
        }
        if c == '"' {
            open_quote = if open_quote.is_some() { None } else { Some(i) };
        }
    }
    let string_error = python_string_error(&text).filter(|(position, _)| {
        *position <= byte_offset
            && (message.starts_with("invalid escape")
                || message.starts_with("invalid unicode")
                || message.starts_with("invalid hex")
                || message.starts_with("EOF while parsing a string")
                || message.starts_with("control character"))
    });
    let reason = if let Some((position, reason)) = string_error {
        offset = position;
        reason
    } else if message.starts_with("EOF while parsing a string") {
        offset = open_quote.unwrap_or(0);
        "Unterminated string starting at"
    } else if message.starts_with("invalid escape") {
        // Invalid escape locations identify the backslash itself, independent
        // of whether the following character reset a line or is multibyte.
        let mut quoted = false;
        let mut characters = text.char_indices();
        while let Some((index, character)) = characters.next() {
            if character == '"' {
                quoted = !quoted;
            } else if quoted
                && character == '\\'
                && let Some((_, next)) = characters.next()
                && !matches!(next, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u')
            {
                offset = index;
                break;
            }
        }
        "Invalid \\escape"
    } else if message.starts_with("invalid unicode escape")
        || message.starts_with("invalid hex escape")
    {
        offset = text[..offset].rfind("\\u").map_or(offset, |i| i + 1);
        "Invalid \\uXXXX escape"
    } else if message.starts_with("control character") {
        // serde_json's cursor for control-byte errors differs from ordinary
        // syntax errors (and LF also resets its line cursor). Locate the actual
        // unescaped control inside the quoted string rather than subtracting
        // one from that cursor. Byte-to-character conversion below then handles
        // multibyte text before the bad byte exactly like CPython.
        let mut quoted = false;
        let mut escaped = false;
        for (index, character) in text.char_indices() {
            if escaped {
                escaped = false;
            } else if quoted && character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = !quoted;
            } else if quoted && character <= '\u{1f}' {
                offset = index;
                break;
            }
        }
        "Invalid control character at"
    } else if message.starts_with("key must be a string")
        || message.starts_with("trailing comma")
        || (message.starts_with("EOF while parsing an object") && text.trim_end().ends_with('{'))
    {
        if !error.is_eof() {
            offset = offset.saturating_sub(1);
        }
        "Expecting property name enclosed in double quotes"
    } else if message.starts_with("expected `:`") {
        offset = offset.saturating_sub(1);
        "Expecting ':' delimiter"
    } else if message.starts_with("expected `,`") {
        offset = offset.saturating_sub(1);
        "Expecting ',' delimiter"
    } else if message.starts_with("trailing characters") {
        offset = offset.saturating_sub(1);
        "Extra data"
    } else {
        if !error.is_eof() {
            offset = offset.saturating_sub(1);
        }
        "Expecting value"
    };
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    ServiceConfigError::validation(vec![
        json!({"type":"json_invalid","loc":["body",text[..offset].chars().count()],"msg":"JSON decode error","input":{},"ctx":{"error":reason}}),
    ])
}
/// Local RawValue decoding avoids changing serde_json Number semantics for the
/// rest of the workspace. Large finite integer tokens need no arithmetic: the
/// disabled endpoint only validates their category, and never consumes them.
fn python_json_integer_overflow(text: &str) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if b[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
        } else if b[i] == b'-' || b[i].is_ascii_digit() {
            let start = i;
            i += 1;
            while i < b.len() && matches!(b[i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-') {
                i += 1;
            }
            let token = &text[start..i];
            let digits = token.strip_prefix('-').unwrap_or(token);
            if digits.bytes().all(|b| b.is_ascii_digit())
                && !digits.starts_with('0')
                && digits.len() > 4300
            {
                return Some(start);
            }
        } else {
            i += 1;
        }
    }
    None
}
fn validation_value(raw: &RawValue) -> Result<Value, serde_json::Error> {
    if let Ok(value) = serde_json::from_str::<Value>(raw.get()) {
        return Ok(value);
    }
    let text = raw.get().trim();
    if text.starts_with('{') {
        let fields: serde_json::Map<String, Value> =
            serde_json::from_str::<HashMap<String, Box<RawValue>>>(text)?
                .into_iter()
                .map(|(k, v)| validation_value(&v).map(|v| (k, v)))
                .collect::<Result<_, _>>()?;
        Ok(Value::Object(fields))
    } else if text.starts_with('[') {
        Ok(Value::Array(
            serde_json::from_str::<Vec<Box<RawValue>>>(text)?
                .iter()
                .map(|v| validation_value(v))
                .collect::<Result<_, _>>()?,
        ))
    } else if text
        .strip_prefix('-')
        .unwrap_or(text)
        .bytes()
        .all(|b| b.is_ascii_digit())
    {
        // Syntax was already checked by RawValue; only an oversized integer
        // arrives here. Keep its original token for error-input serialization.
        Ok(json!(0))
    } else {
        serde_json::from_str(text)
    }
}
fn canonical_raw_input(raw: &RawValue) -> Result<Box<RawValue>, serde_json::Error> {
    let text = raw.get().trim();
    let serialized = if text.starts_with('{') {
        let fields: std::collections::BTreeMap<String, Box<RawValue>> = serde_json::from_str(text)?;
        let entries = fields
            .into_iter()
            .map(|(key, value)| {
                Ok(format!(
                    "{}:{}",
                    serde_json::to_string(&key)?,
                    canonical_raw_input(&value)?.get()
                ))
            })
            .collect::<Result<Vec<_>, serde_json::Error>>()?;
        format!("{{{}}}", entries.join(","))
    } else if text.starts_with('[') {
        let values: Vec<Box<RawValue>> = serde_json::from_str(text)?;
        let entries = values
            .iter()
            .map(|v| canonical_raw_input(v).map(|v| v.get().to_owned()))
            .collect::<Result<Vec<_>, _>>()?;
        format!("[{}]", entries.join(","))
    } else {
        text.to_owned()
    };
    RawValue::from_string(serialized)
}
fn raw_input(raw: &RawValue, loc: &Value) -> Option<Box<RawValue>> {
    let mut value = RawValue::from_string(raw.get().to_owned()).ok()?;
    for part in loc.as_array()?.iter().skip(1) {
        if let Some(key) = part.as_str() {
            let mut fields: HashMap<String, Box<RawValue>> =
                serde_json::from_str(value.get()).ok()?;
            value = fields.remove(key)?;
        } else {
            let index = part.as_u64()?;
            let mut values: Vec<Box<RawValue>> = serde_json::from_str(value.get()).ok()?;
            if index as usize >= values.len() {
                return None;
            }
            value = values.remove(index as usize);
        }
    }
    canonical_raw_input(&value).ok()
}
/// Preserve original numeric JSON types in validation input without adding a
/// global arbitrary_precision feature or converting numeric values to strings.
fn raw_validation_response(error: &ServiceConfigError, raw: &RawValue) -> Option<Response> {
    let ServiceConfigError::Http { status: 422, body } = error else {
        return None;
    };
    let errors = body.get("detail")?.as_array()?;
    let mut serialized = Vec::with_capacity(errors.len());
    for error in errors {
        let mut value = error.clone();
        // Missing fields echo the entire input object, not an absent field.
        let loc = if error["type"] == "missing" {
            json!(["body"])
        } else {
            error["loc"].clone()
        };
        let raw = raw_input(raw, &loc)?;
        value.as_object_mut()?.remove("input");
        let mut text = serde_json::to_string(&value).ok()?;
        text.pop();
        text.push_str(",\"input\":");
        text.push_str(raw.get());
        text.push('}');
        serialized.push(text);
    }
    Some(
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            [("content-type", "application/json")],
            format!("{{\"detail\":[{}]}}", serialized.join(",")),
        )
            .into_response(),
    )
}
async fn dispatch(
    State(state): State<HttpState>,
    matched: MatchedPath,
    params: Result<Path<HashMap<String, String>>, axum::extract::rejection::PathRejection>,
    request: Request,
) -> Response {
    let params = match params {
        Ok(Path(p)) => p,
        Err(e) => return e.into_response(),
    };
    if params.values().any(|s| s.contains('/')) {
        return (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response();
    }
    let operation = if matched.as_str().ends_with("/upload") {
        PresignOperation::Upload
    } else if matched.as_str().ends_with("/confirm") {
        PresignOperation::Confirm
    } else {
        PresignOperation::Status
    };
    let method = if operation.body() { "POST" } else { "GET" };
    if request.method().as_str() != method {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", method)],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response();
    }
    let headers = request.headers().clone();
    let path = request.uri().path().to_owned();
    let mut raw_body: Option<Box<RawValue>> = None;
    let body = if operation.body() {
        let bytes = match Bytes::from_request(request, &state).await {
            Ok(b) => b,
            Err(e) => return e.into_response(),
        };
        if bytes.is_empty() {
            Value::Null
        } else if crate::http_content_type::is_json_content_type(&headers) {
            let raw = match serde_json::from_slice::<Box<RawValue>>(&bytes) {
                Ok(v) => v,
                Err(e) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let position = text
                        .split_inclusive('\n')
                        .take(e.line().saturating_sub(1))
                        .map(str::len)
                        .sum::<usize>()
                        + e.column().saturating_sub(1);
                    if python_json_integer_overflow(&text).is_some_and(|start| start < position) {
                        return respond(
                            &state,
                            Err(ServiceConfigError::detail(
                                400,
                                "There was an error parsing the body",
                            )),
                            &headers,
                        )
                        .await;
                    }
                    return respond(&state, Err(json_error(&bytes, e)), &headers).await;
                }
            };
            if python_json_integer_overflow(raw.get()).is_some() {
                return respond(
                    &state,
                    Err(ServiceConfigError::detail(
                        400,
                        "There was an error parsing the body",
                    )),
                    &headers,
                )
                .await;
            }
            let value = match validation_value(&raw) {
                Ok(v) => v,
                Err(e) => return respond(&state, Err(json_error(&bytes, e)), &headers).await,
            };
            raw_body = Some(raw);
            value
        } else {
            json!(String::from_utf8_lossy(&bytes))
        }
    } else {
        Value::Null
    };
    let identity = match state.api.identity.resolve(&headers, &path).await {
        Ok(v) => v,
        Err(e) => return respond(&state, Err(e), &headers).await,
    };
    let result = state.api.execute(operation, &identity, &body);
    if let (Err(error), Some(raw)) = (&result, &raw_body)
        && let Some(response) = raw_validation_response(error, raw)
    {
        return response;
    }
    respond(&state, result, &headers).await
}
