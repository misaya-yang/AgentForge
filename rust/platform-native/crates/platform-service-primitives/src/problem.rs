//! RFC 9457 envelopes with lossless support for legacy structured `detail`.
//! Structured legacy details stay application/json; strict string details use
//! application/problem+json. Provider protocol envelopes are never rewritten.
use axum::{
    Json,
    http::{HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProblemDetails {
    #[serde(rename = "type")]
    pub problem_type: String,
    pub title: String,
    pub status: u16,
    pub detail: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    #[serde(flatten)]
    pub extensions: serde_json::Map<String, Value>,
}
impl ProblemDetails {
    pub fn new(status: StatusCode, detail: impl Into<Value>) -> Self {
        Self {
            problem_type: problem_type(status).into(),
            title: status.canonical_reason().unwrap_or("HTTP error").into(),
            status: status.as_u16(),
            detail: detail.into(),
            instance: None,
            extensions: Default::default(),
        }
    }
    pub fn from_legacy(status: StatusCode, mut body: Value) -> Option<Self> {
        let object = body.as_object_mut()?;
        let detail = object.remove("detail")?;
        let mut problem = Self::new(status, detail);
        // Status/type/title are server-owned, never taken from arbitrary details.
        for key in ["status", "type", "title", "instance"] {
            object.remove(key);
        }
        problem.extensions = std::mem::take(object);
        Some(problem)
    }
    pub fn strict(&self) -> bool {
        self.detail.is_string()
    }
}
impl IntoResponse for ProblemDetails {
    fn into_response(self) -> Response {
        let strict = self.strict();
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut response = (status, Json(self)).into_response();
        if strict {
            response.headers_mut().insert(
                CONTENT_TYPE,
                HeaderValue::from_static("application/problem+json"),
            );
        }
        response
    }
}
pub fn problem_type(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 => "urn:agentforge:problem:invalid-request",
        401 => "urn:agentforge:problem:authentication-required",
        403 => "urn:agentforge:problem:permission-denied",
        404 => "urn:agentforge:problem:not-found",
        409 => "urn:agentforge:problem:conflict",
        413 => "urn:agentforge:problem:request-too-large",
        422 => "urn:agentforge:problem:validation",
        429 => "urn:agentforge:problem:rate-limit",
        502 => "urn:agentforge:problem:upstream",
        503 => "urn:agentforge:problem:unavailable",
        504 => "urn:agentforge:problem:timeout",
        _ => "about:blank",
    }
}
pub fn schema() -> Value {
    json!({"type":"object","required":["type","title","status","detail"],"properties":{
        "type":{"type":"string","format":"uri-reference"},"title":{"type":"string"},
        "status":{"type":"integer","minimum":400,"maximum":599},"detail":{"type":"string"},
        "instance":{"type":"string","format":"uri-reference"}},"additionalProperties":true})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_detail_is_not_flattened_or_relabelled_as_rfc9457() {
        let detail = json!({"message":"Permission denied","accepted_permissions":["console:read"]});
        let problem = ProblemDetails::from_legacy(
            StatusCode::FORBIDDEN,
            json!({"detail":detail,"trace_id":"t"}),
        )
        .unwrap();
        assert_eq!(problem.detail, detail);
        assert!(!problem.strict());
        assert_eq!(problem.extensions["trace_id"], "t");
    }
    #[test]
    fn provider_error_envelopes_are_not_rewritten() {
        assert!(
            ProblemDetails::from_legacy(
                StatusCode::BAD_REQUEST,
                json!({"error":{"type":"invalid_request_error"}})
            )
            .is_none()
        );
    }
    #[test]
    fn canonical_status_cannot_be_spoofed_by_legacy_body() {
        let problem = ProblemDetails::from_legacy(StatusCode::UNAUTHORIZED, json!({"detail":"No token","status":200,"type":"https://attacker.invalid","title":"ok"})).unwrap();
        assert_eq!(problem.status, 401);
        assert_eq!(problem.problem_type, problem_type(StatusCode::UNAUTHORIZED));
    }
}
