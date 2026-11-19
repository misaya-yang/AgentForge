//! Opt-in public Local Node control plane, including confirmed owner stop and audit.
//! Construction is inert. The host provides canonical identity, a real durable
//! control plane, and (for completion) a trusted device-channel verifier. No
//! capability/admin gate or successful fallback verifier is invented here.
use crate::{
    local_nodes_types::{
        LocalNodesControlPlane, LocalNodesError, LocalNodesFuture, LocalNodesIdentity,
    },
    local_nodes_validation::{LocalNodesBodyKind, validate_after_sequence, validate_body},
};
use ai_platform_auth::admission::AdmissionService;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{FromRequest, MatchedPath, Path, Request, State},
    http::{Extensions, HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

/// Identity must be derived by a trusted host resolver, never directly from
/// caller-controlled tenant/user headers. Anonymous source identities are allowed.
pub trait LocalNodesIdentityResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> LocalNodesFuture<'a, LocalNodesIdentity>;
}
impl LocalNodesIdentityResolver for AdmissionService {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> LocalNodesFuture<'a, LocalNodesIdentity> {
        Box::pin(async move {
            let request = self.request(headers, path);
            let user = request
                .resolve()
                .await
                .map_err(|e| LocalNodesError::detail(e.status(), e.to_string()))?;
            Ok(LocalNodesIdentity {
                tenant_id: user.tenant_id().into(),
                user_id: user.user_id().into(),
            })
        })
    }
}
/// Trusted host request context. Extensions retain authenticated channel/peer
/// metadata without converting untrusted request JSON into a verified channel.
#[derive(Clone, Debug)]
pub struct LocalNodesRequestContext {
    pub method: Method,
    pub uri: Uri,
    pub headers: HeaderMap,
    pub extensions: Extensions,
}
pub trait LocalNodesChannelVerifier: Send + Sync {
    fn verify<'a>(
        &'a self,
        request: &'a LocalNodesRequestContext,
        purpose: &'a str,
        challenge_id: Option<&'a str>,
        expected_device_id: Option<&'a str>,
        body: Value,
    ) -> LocalNodesFuture<'a, Value>;
}
#[derive(Clone, Default)]
pub struct LocalNodesApi {
    pub control_plane: Option<Arc<dyn LocalNodesControlPlane>>,
    pub channel_verifier: Option<Arc<dyn LocalNodesChannelVerifier>>,
}
impl LocalNodesApi {
    /// Only errors raised inside the canonical service-call boundary receive
    /// repository-to-409 translation. Verifier and identity errors do not.
    pub async fn call(
        &self,
        operation: &str,
        owner: &LocalNodesIdentity,
        args: Value,
    ) -> Result<Value, LocalNodesError> {
        let service = self
            .control_plane
            .as_ref()
            .ok_or_else(|| LocalNodesError::detail(503, "Local Node control plane unavailable"))?;
        service
            .call(operation, owner, args)
            .await
            .map_err(|error| match error {
                LocalNodesError::Repository(detail) => LocalNodesError::detail(409, detail),
                other => other,
            })
    }
    pub async fn complete(
        &self,
        request: &LocalNodesRequestContext,
        owner: &LocalNodesIdentity,
        challenge_id: &str,
        body: Value,
    ) -> Result<Value, LocalNodesError> {
        let verifier = self.channel_verifier.as_ref().ok_or_else(|| {
            LocalNodesError::detail(503, "Local Node channel verifier unavailable")
        })?;
        let channel = verifier
            .verify(
                request,
                "pairing.complete",
                Some(challenge_id),
                None,
                body.clone(),
            )
            .await?;
        let mut args = body;
        args["challenge_id"] = json!(challenge_id);
        args["channel"] = channel;
        self.call("pairing.complete", owner, args).await
    }
}
/// Method, suffix and canonical operation. Aliases do not double the operation count.
pub const LOCAL_NODES_ROUTES: &[(&str, &str, &str)] = &[
    ("POST", "/pairing/challenges", "pairing.create"),
    (
        "POST",
        "/pairing/challenges/{challenge_id}/complete",
        "pairing.complete",
    ),
    ("GET", "", "devices.list"),
    ("GET", "/{device_id}/status", "device.status"),
    ("GET", "/{device_id}/capabilities", "device.capabilities"),
    ("GET", "/{device_id}/doctor", "device.doctor"),
    ("GET", "/{device_id}/grants", "grants.list"),
    ("POST", "/{device_id}/grants", "grants.create"),
    ("DELETE", "/{device_id}/grants/{grant_id}", "grants.revoke"),
    ("GET", "/{device_id}/events", "events.list"),
    (
        "POST",
        "/{device_id}/emergency-stop",
        "device.emergency_stop",
    ),
    (
        "GET",
        "/{device_id}/controls/{command_id}",
        "controls.status",
    ),
];
#[derive(Clone)]
struct HttpState {
    api: LocalNodesApi,
    identity: Arc<dyn LocalNodesIdentityResolver>,
}
/// No listener, environment reads, connection attempts or service startup.
/// Hosts own request-size limits and trusted forwarded-origin normalization.
pub fn local_nodes_router(
    api: LocalNodesApi,
    identity: Arc<dyn LocalNodesIdentityResolver>,
) -> Router {
    let mut router = Router::new();
    for base in ["/api/v1/local-nodes", "/api/v1/assistant/local-nodes"] {
        let mut seen = std::collections::HashSet::new();
        for (_, suffix, _) in LOCAL_NODES_ROUTES {
            if seen.insert(*suffix) {
                let path = format!("{base}{suffix}");
                router = router
                    .route(&path, any(handle))
                    .route(&format!("{path}/"), any(redirect_slash));
            }
        }
    }
    router.with_state(HttpState { api, identity })
}
async fn redirect_slash(request: Request) -> Response {
    let uri = request.uri();
    let path = uri.path().trim_end_matches('/');
    let target = match uri.query() {
        Some(query) => format!("{path}?{query}"),
        None => path.to_owned(),
    };
    // As in Starlette, use the request's effective origin. Reverse proxies must
    // normalize trusted forwarded headers before this router, not raw headers here.
    let scheme = uri.scheme_str().unwrap_or("http");
    let authority = uri
        .authority()
        .map(|a| a.as_str())
        .or_else(|| request.headers().get("host").and_then(|v| v.to_str().ok()));
    let location = authority
        .map(|host| format!("{scheme}://{host}{target}"))
        .unwrap_or(target);
    (StatusCode::TEMPORARY_REDIRECT, [("location", location)]).into_response()
}
async fn respond(result: Result<Value, LocalNodesError>, status: StatusCode) -> Response {
    match result {
        Ok(value) => (status, Json(value)).into_response(),
        Err(LocalNodesError::Http { status, detail }) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"detail":detail})),
        )
            .into_response(),
        Err(LocalNodesError::Repository(_) | LocalNodesError::Internal) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
        }
        Err(LocalNodesError::Cancelled) => std::future::pending().await,
    }
}
fn decode_query(raw: &str) -> HashMap<String, String> {
    fn decode(raw: &str) -> String {
        let bytes = raw.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'+' {
                out.push(b' ');
            } else if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hex = |b: u8| (b as char).to_digit(16);
                if let (Some(a), Some(b)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    out.push((a * 16 + b) as u8);
                    i += 3;
                    continue;
                } else {
                    out.push(bytes[i]);
                }
            } else {
                out.push(bytes[i]);
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    raw.split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            (decode(key), decode(value))
        })
        .collect()
}
// Python JSONDecoder's syntax diagnostics use Unicode character offsets. A
// narrow scanner avoids leaking serde's different wording and byte positions.
struct JsonSyntax<'a> {
    bytes: &'a [u8],
    pos: usize,
    nonfinite: Vec<(usize, usize)>,
    strings: HashSet<String>,
}
impl JsonSyntax<'_> {
    fn whitespace(&mut self) {
        while self
            .bytes
            .get(self.pos)
            .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.pos += 1;
        }
    }
    fn fail(&self, reason: &'static str) -> (usize, &'static str) {
        (self.pos, reason)
    }
    fn string(&mut self) -> Result<(), (usize, &'static str)> {
        let start = self.pos;
        self.pos += 1;
        loop {
            match self.bytes.get(self.pos).copied() {
                None => return Err((start, "Unterminated string starting at")),
                Some(b'"') => {
                    self.pos += 1;
                    if let Ok(text) = serde_json::from_slice::<String>(&self.bytes[start..self.pos])
                    {
                        self.strings.insert(text);
                    }
                    return Ok(());
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.bytes.get(self.pos).copied() {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {
                            self.pos += 1
                        }
                        Some(b'u') => {
                            let escape = self.pos;
                            self.pos += 1;
                            for _ in 0..4 {
                                if !self.bytes.get(self.pos).is_some_and(u8::is_ascii_hexdigit) {
                                    return Err((escape, "Invalid \\uXXXX escape"));
                                }
                                self.pos += 1;
                            }
                        }
                        None => return Err((start, "Unterminated string starting at")),
                        _ => return Err((self.pos.saturating_sub(1), "Invalid \\escape")),
                    }
                }
                Some(b) if b < 0x20 => return Err(self.fail("Invalid control character at")),
                Some(_) => self.pos += 1,
            }
        }
    }
    fn value(&mut self) -> Result<(), (usize, &'static str)> {
        self.whitespace();
        match self.bytes.get(self.pos).copied() {
            Some(b'"') => self.string(),
            Some(b'{') => {
                self.pos += 1;
                self.whitespace();
                if self.bytes.get(self.pos) == Some(&b'}') {
                    self.pos += 1;
                    return Ok(());
                }
                loop {
                    if self.bytes.get(self.pos) != Some(&b'"') {
                        return Err(self.fail("Expecting property name enclosed in double quotes"));
                    }
                    self.string()?;
                    self.whitespace();
                    if self.bytes.get(self.pos) != Some(&b':') {
                        return Err(self.fail("Expecting ':' delimiter"));
                    }
                    self.pos += 1;
                    self.value()?;
                    self.whitespace();
                    match self.bytes.get(self.pos) {
                        Some(b'}') => {
                            self.pos += 1;
                            return Ok(());
                        }
                        Some(b',') => {
                            self.pos += 1;
                            self.whitespace();
                        }
                        _ => return Err(self.fail("Expecting ',' delimiter")),
                    }
                }
            }
            Some(b'[') => {
                self.pos += 1;
                self.whitespace();
                if self.bytes.get(self.pos) == Some(&b']') {
                    self.pos += 1;
                    return Ok(());
                }
                loop {
                    self.value()?;
                    self.whitespace();
                    match self.bytes.get(self.pos) {
                        Some(b']') => {
                            self.pos += 1;
                            return Ok(());
                        }
                        Some(b',') => {
                            self.pos += 1;
                            self.whitespace();
                        }
                        _ => return Err(self.fail("Expecting ',' delimiter")),
                    }
                }
            }
            _ => {
                for token in ["true", "false", "null", "NaN", "Infinity", "-Infinity"] {
                    if self.bytes[self.pos..].starts_with(token.as_bytes()) {
                        self.pos += token.len();
                        if matches!(token, "NaN" | "Infinity" | "-Infinity") {
                            self.nonfinite.push((self.pos - token.len(), self.pos));
                        }
                        return Ok(());
                    }
                }
                let start = self.pos;
                if self.bytes.get(self.pos) == Some(&b'-') {
                    self.pos += 1;
                }
                match self.bytes.get(self.pos) {
                    Some(b'0') => self.pos += 1,
                    Some(b'1'..=b'9') => {
                        self.pos += 1;
                        while self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
                            self.pos += 1;
                        }
                    }
                    _ => return Err((start, "Expecting value")),
                }
                if self.bytes.get(self.pos) == Some(&b'.')
                    && self.bytes.get(self.pos + 1).is_some_and(u8::is_ascii_digit)
                {
                    self.pos += 2;
                    while self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
                        self.pos += 1;
                    }
                }
                if self
                    .bytes
                    .get(self.pos)
                    .is_some_and(|b| matches!(b, b'e' | b'E'))
                {
                    let exponent = self.pos;
                    self.pos += 1;
                    if self
                        .bytes
                        .get(self.pos)
                        .is_some_and(|b| matches!(b, b'+' | b'-'))
                    {
                        self.pos += 1;
                    }
                    if !self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
                        self.pos = exponent;
                    } else {
                        while self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
                            self.pos += 1;
                        }
                    }
                }
                let number = &self.bytes[start..self.pos];
                if number.iter().any(|b| matches!(b, b'.' | b'e' | b'E'))
                    && std::str::from_utf8(number)
                        .ok()
                        .and_then(|n| n.parse::<f64>().ok())
                        .is_some_and(|n| !n.is_finite())
                {
                    self.nonfinite.push((start, self.pos));
                }
                Ok(())
            }
        }
    }
}
pub(crate) fn json_body(
    bytes: &[u8],
    headers: &HeaderMap,
) -> Result<(Value, bool), LocalNodesError> {
    if bytes.is_empty() {
        return Ok((Value::Null, false));
    }
    if !crate::http_content_type::is_json_content_type(headers) {
        return Ok((json!(String::from_utf8_lossy(bytes)), false));
    }
    if std::str::from_utf8(bytes).is_err() {
        return Err(LocalNodesError::detail(
            400,
            "There was an error parsing the body",
        ));
    }
    // Match the bounded native value decoder's depth safety boundary before
    // recursing. Arbitrarily deep JSON is outside the frozen wire fixture scope.
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    for &byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > 128 {
                        return Err(LocalNodesError::Internal);
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    let mut syntax = JsonSyntax {
        bytes,
        pos: 0,
        nonfinite: vec![],
        strings: HashSet::new(),
    };
    let result = syntax.value().and_then(|()| {
        syntax.whitespace();
        if syntax.pos < bytes.len() {
            Err(syntax.fail("Extra data"))
        } else {
            Ok(())
        }
    });
    if let Err((offset, reason)) = result {
        let offset = String::from_utf8_lossy(&bytes[..offset.min(bytes.len())])
            .chars()
            .count();
        return Err(LocalNodesError::detail(
            422,
            json!([{"type":"json_invalid","loc":["body",offset],"msg":"JSON decode error","input":{},"ctx":{"error":reason}}]),
        ));
    }
    // Python accepts nonfinite values, then discards overwritten duplicate
    // object fields. Replace tokens with a collision-free internal sentinel,
    // apply serde's last-key-wins object decoding, and only then inspect survivors.
    // The sentinel is absent from every decoded input string, including escaped
    // equivalents, so an ordinary caller string cannot be misclassified.
    let sentinel = (0..=syntax.strings.len())
        .map(|index| format!("\0local_nodes_nonfinite:{index}"))
        .find(|candidate| !syntax.strings.contains(candidate))
        .ok_or(LocalNodesError::Internal)?;
    let replacement = serde_json::to_vec(&sentinel).map_err(|_| LocalNodesError::Internal)?;
    let mut rewritten = Vec::new();
    let mut offset = 0;
    for (start, end) in &syntax.nonfinite {
        rewritten.extend_from_slice(&bytes[offset..*start]);
        rewritten.extend_from_slice(&replacement);
        offset = *end;
    }
    rewritten.extend_from_slice(&bytes[offset..]);
    fn retained(value: &Value, sentinel: &str) -> bool {
        match value {
            Value::String(s) => s == sentinel,
            Value::Array(items) => items.iter().any(|item| retained(item, sentinel)),
            Value::Object(items) => items.values().any(|item| retained(item, sentinel)),
            _ => false,
        }
    }
    match serde_json::from_slice(&rewritten) {
        Ok(value) => {
            let nonfinite = retained(&value, &sentinel);
            Ok((value, nonfinite))
        }
        Err(_) => Ok((Value::Null, true)),
    }
}
async fn handle(
    State(state): State<HttpState>,
    matched: MatchedPath,
    Path(params): Path<HashMap<String, String>>,
    request: Request,
) -> Response {
    if params.values().any(|value| value.contains('/')) {
        return (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response();
    }
    let suffix = matched
        .as_str()
        .strip_prefix("/api/v1/assistant/local-nodes")
        .or_else(|| matched.as_str().strip_prefix("/api/v1/local-nodes"))
        .unwrap_or("");
    let Some((_, _, operation)) = LOCAL_NODES_ROUTES
        .iter()
        .find(|(method, path, _)| *method == request.method().as_str() && *path == suffix)
    else {
        let allow = LOCAL_NODES_ROUTES
            .iter()
            .find(|(_, path, _)| *path == suffix)
            .map(|(method, _, _)| *method)
            .unwrap_or("");
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", allow)],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response();
    };
    let operation = *operation;
    let body_kind = match operation {
        "pairing.create" => Some(LocalNodesBodyKind::Challenge),
        "pairing.complete" => Some(LocalNodesBodyKind::Completion),
        "grants.create" => Some(LocalNodesBodyKind::Grant),
        "device.emergency_stop" => Some(LocalNodesBodyKind::EmergencyStop),
        _ => None,
    };
    let status = if operation == "device.emergency_stop" {
        StatusCode::ACCEPTED
    } else if body_kind.is_some() {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let context = LocalNodesRequestContext {
        method: request.method().clone(),
        uri: request.uri().clone(),
        headers: request.headers().clone(),
        extensions: request.extensions().clone(),
    };
    let query = decode_query(request.uri().query().unwrap_or(""));
    let (raw, unrepresentable) = if body_kind.is_some() {
        let bytes = match Bytes::from_request(request, &state).await {
            Ok(v) => v,
            Err(e) => return e.into_response(),
        };
        match json_body(&bytes, &context.headers) {
            Ok(v) => v,
            Err(e) => return respond(Err(e), status).await,
        }
    } else {
        (Value::Null, false)
    };
    let result = async {
        let owner = state
            .identity
            .resolve(&context.headers, context.uri.path())
            .await?;
        if unrepresentable {
            return Err(LocalNodesError::Internal);
        }
        let mut args = if let Some(kind) = body_kind {
            validate_body(kind, &raw)?
        } else {
            json!({})
        };
        if operation == "pairing.complete" {
            return state
                .api
                .complete(
                    &context,
                    &owner,
                    params.get("challenge_id").map(String::as_str).unwrap_or(""),
                    args,
                )
                .await;
        }
        if let Some(id) = params.get("device_id") {
            args["device_id"] = json!(id);
        }
        if let Some(id) = params.get("grant_id") {
            args["grant_id"] = json!(id);
        }
        if let Some(id) = params.get("command_id") {
            args["command_id"] = json!(id);
        }
        if operation == "events.list" {
            args["after_sequence"] = json!(validate_after_sequence(
                query.get("after_sequence").map(String::as_str)
            )?);
        }
        state.api.call(operation, &owner, args).await
    }
    .await;
    respond(result, status).await
}
