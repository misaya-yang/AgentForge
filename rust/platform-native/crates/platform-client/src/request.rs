//! Shared authenticated JSON transport for native console and management CLI.
use crate::{Client, Error};
use reqwest::Method;
use serde_json::Value;
use std::time::Duration;

#[derive(Clone)]
pub struct ApiRequest {
    pub method: Method,
    pub segments: Vec<String>,
    pub query: Vec<(String, String)>,
    pub body: Option<Value>,
    pub headers: Vec<(String, String)>,
}
impl ApiRequest {
    pub fn get(segments: &[&str]) -> Self {
        Self {
            method: Method::GET,
            segments: segments.iter().map(|s| (*s).into()).collect(),
            query: Vec::new(),
            body: None,
            headers: Vec::new(),
        }
    }
    pub fn mutation(method: Method, segments: &[&str], body: Option<Value>) -> Self {
        Self {
            method,
            body,
            ..Self::get(segments)
        }
    }
    pub fn query(mut self, key: &str, value: impl ToString) -> Self {
        self.query.push((key.into(), value.to_string()));
        self
    }
    pub fn header(mut self, name: &str, value: impl ToString) -> Self {
        self.headers.push((name.into(), value.to_string()));
        self
    }
    pub fn is_mutation(&self) -> bool {
        self.method != Method::GET && self.method != Method::HEAD
    }
}
impl Client {
    /// One attempt, with no redirects or automatic retry of a mutation.
    pub async fn execute(&self, request: ApiRequest) -> Result<Value, Error> {
        self.execute_with_timeout(request, self.default_timeout)
            .await
    }
    pub async fn execute_with_timeout(
        &self,
        request: ApiRequest,
        timeout: Duration,
    ) -> Result<Value, Error> {
        if timeout < Duration::from_secs(1)
            || timeout > Duration::from_secs(300)
            || request.segments.len() < 3
            || request.segments[0] != "api"
            || !["v1", "v2"].contains(&request.segments[1].as_str())
            || request.segments.iter().any(|s| {
                s.is_empty() || s == "." || s == ".." || s.contains(['\\', '\r', '\n', '\0'])
            })
            || ![
                Method::GET,
                Method::HEAD,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
            ]
            .contains(&request.method)
        {
            return Err(Error::Request);
        }
        let mut url = self.origin.clone();
        // Segments are encoded individually; resource names can contain a slash.
        url.path_segments_mut()
            .map_err(|_| Error::Origin)?
            .clear()
            .extend(&request.segments);
        let read_only = request.method == Method::GET || request.method == Method::HEAD;
        let mut pending = self
            .authorize(self.http.request(request.method, url))?
            .query(&request.query)
            .timeout(timeout);
        for (name, value) in request.headers {
            if !["if-match", "idempotency-key"].contains(&name.to_ascii_lowercase().as_str())
                || value.contains(['\r', '\n'])
            {
                return Err(Error::Request);
            }
            pending = pending.header(name, value);
        }
        if let Some(body) = request.body {
            pending = pending.json(&body);
        }
        let mut response = self.send_request(pending, read_only).await?;
        if !response.status().is_success() {
            return Err(crate::problem::decode_response(response).await);
        }
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Transport)? {
            if bytes.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
                return Err(Error::Response);
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() {
            Ok(Value::Null)
        } else {
            serde_json::from_slice(&bytes).map_err(|_| Error::Response)
        }
    }
}
