//! Typed remote error projection. Debug/Display never render untrusted details.
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Serialize, Deserialize)]
pub struct RemoteProblem {
    #[serde(rename = "type")]
    pub problem_type: String,
    pub title: String,
    pub status: u16,
    pub detail: Value,
    #[serde(default)]
    pub instance: Option<String>,
}
impl std::fmt::Debug for RemoteProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteProblem")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for RemoteProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Server returned HTTP {}", self.status)
    }
}
impl RemoteProblem {
    pub fn decode(status: u16, body: &[u8]) -> Option<Self> {
        if body.len() > 64 * 1024 || !(400..600).contains(&status) {
            return None;
        }
        let problem: Self = serde_json::from_slice(body).ok()?;
        (problem.status == status
            && problem.problem_type.len() <= 1024
            && problem.title.len() <= 256
            && (problem.problem_type == "about:blank"
                || problem.problem_type.starts_with("urn:agentforge:problem:")
                || problem.problem_type.starts_with("https://")))
        .then_some(problem)
    }
}
pub async fn decode_response(mut response: reqwest::Response) -> crate::Error {
    let status = response.status().as_u16();
    let media = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("");
    if !matches!(media, "application/problem+json" | "application/json") {
        return crate::Error::Status(status);
    }
    if response.content_length().is_some_and(|n| n > 64 * 1024) {
        return crate::Error::Status(status);
    }
    let mut bytes = Vec::new();
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) => return crate::Error::Status(status),
        };
        if bytes.len() + chunk.len() > 64 * 1024 {
            return crate::Error::Status(status);
        }
        bytes.extend_from_slice(&chunk);
    }
    RemoteProblem::decode(status, &bytes)
        .map(|problem| crate::Error::Problem(Box::new(problem)))
        .unwrap_or(crate::Error::Status(status))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_or_spoofed_status_does_not_create_typed_problem() {
        assert!(
            RemoteProblem::decode(
                403,
                br#"{"type":"about:blank","title":"x","detail":"x","status":200}"#
            )
            .is_none()
        );
        assert!(RemoteProblem::decode(403, br#"{"detail":"legacy"}"#).is_none());
    }
    #[test]
    fn legacy_structured_detail_is_preserved_and_debug_is_redacted() {
        let problem=RemoteProblem::decode(403,br#"{"type":"urn:agentforge:problem:permission-denied","title":"Forbidden","status":403,"detail":{"message":"secret-marker","required_permission":"x"}}"#).unwrap();
        assert_eq!(problem.detail["required_permission"], "x");
        assert!(!format!("{problem:?}").contains("secret-marker"));
        assert!(!problem.to_string().contains("secret-marker"));
    }
}
