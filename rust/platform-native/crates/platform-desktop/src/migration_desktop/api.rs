use reqwest::{Client, Method, Url};
use serde_json::{Value, json};
use std::{fmt, net::IpAddr, sync::Arc, time::Duration};
#[derive(Clone, Debug)]
pub enum ApiError {
    Origin,
    Authentication,
    Request,
    Transport,
    Response,
    Contract(&'static str),
    Status(u16),
}
impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Origin => f.write_str("Use an HTTPS origin or literal loopback HTTP origin"),
            Self::Authentication => f.write_str("Authentication required"),
            Self::Request => f.write_str("Invalid request"),
            Self::Transport => {
                f.write_str("Request failed; a submitted change may have an unknown outcome")
            }
            Self::Response => f.write_str("Invalid or oversized response"),
            Self::Contract(message) => f.write_str(message),
            Self::Status(s) => write!(f, "Server returned HTTP {s}"),
        }
    }
}
#[derive(Clone)]
pub struct DesktopApi {
    origin: Url,
    http: Client,
    pub(super) token: Arc<str>,
    shared: ai_platform_client::Client,
}
pub use ai_platform_client::ApiRequest;
impl From<ai_platform_client::Error> for ApiError {
    fn from(error: ai_platform_client::Error) -> Self {
        match error {
            ai_platform_client::Error::UnknownOutcome => {
                Self::Contract("Submission outcome unknown; do not automatically resubmit")
            }
            ai_platform_client::Error::Origin => Self::Origin,
            ai_platform_client::Error::Request => Self::Request,
            ai_platform_client::Error::Authentication => Self::Authentication,
            ai_platform_client::Error::Transport => Self::Transport,
            ai_platform_client::Error::Response => Self::Response,
            ai_platform_client::Error::Contract(message) => Self::Contract(message),
            ai_platform_client::Error::Status(status) => Self::Status(status),
            ai_platform_client::Error::Problem(problem) => Self::Status(problem.status),
        }
    }
}
#[cfg(test)]
mod error_tests {
    use super::*;

    #[test]
    fn structured_errors_preserve_desktop_status_branches_without_rendering_details() {
        for status in [401, 403, 404, 409, 429, 503] {
            let problem = ai_platform_client::problem::RemoteProblem {
                problem_type: "about:blank".into(),
                title: "private-marker".into(),
                status,
                detail: json!({"message": "private-marker"}),
                instance: None,
            };
            let error = ApiError::from(ai_platform_client::Error::Problem(Box::new(problem)));
            assert!(matches!(error, ApiError::Status(actual) if actual == status));
            assert_eq!(error.to_string(), format!("Server returned HTTP {status}"));
            assert!(!format!("{error:?}").contains("private-marker"));
        }
    }
}
impl DesktopApi {
    /// In-memory session fence only; credentials are never serialized.
    pub fn same_session(&self, other: &Self) -> bool {
        self.origin == other.origin && self.token == other.token
    }
    pub fn new(origin: &str, token: String) -> Result<Self, ApiError> {
        let origin = Url::parse(origin).map_err(|_| ApiError::Origin)?;
        let local = origin
            .host_str()
            .and_then(|s| s.trim_matches(['[', ']']).parse::<IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback());
        if origin.host_str().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
            || origin.path() != "/"
            || !(origin.scheme() == "https" || (origin.scheme() == "http" && local))
        {
            return Err(ApiError::Origin);
        }
        if token.is_empty() || token.contains(['\r', '\n']) {
            return Err(ApiError::Authentication);
        }
        let http = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| ApiError::Transport)?;
        let shared = ai_platform_client::Client::new(origin.as_str())
            .map_err(ApiError::from)?
            .authenticated(token.clone());
        Ok(Self {
            shared,
            origin,
            http,
            token: token.into(),
        })
    }
    pub fn endpoint(&self, segments: &[String]) -> Result<Url, ApiError> {
        if segments.len() < 3
            || segments[0] != "api"
            || !["v1", "v2"].contains(&segments[1].as_str())
            || segments.iter().any(|s| {
                s.is_empty() || s == "." || s == ".." || s.contains(['\\', '\r', '\n', '\0'])
            })
        {
            return Err(ApiError::Request);
        }
        let mut url = self.origin.clone();
        url.path_segments_mut()
            .map_err(|_| ApiError::Origin)?
            .clear()
            .extend(segments);
        Ok(url)
    }
    /// Uses the same bounded transport and mutation rules as platformctl.
    pub async fn execute(&self, request: ApiRequest) -> Result<Value, ApiError> {
        self.shared.execute(request).await.map_err(ApiError::from)
    }
    pub fn runtime_client(&self) -> Result<ai_platform_client::Client, ApiError> {
        Ok(self.shared.clone())
    }
    pub async fn local_nodes(&self) -> Result<Value, ApiError> {
        self.execute(ApiRequest::get(&["api", "v1", "local-nodes"]))
            .await
    }
    pub async fn local_node(&self, id: &str, suffix: &str) -> Result<Value, ApiError> {
        if !["status", "capabilities", "doctor", "grants", "events"].contains(&suffix) {
            return Err(ApiError::Request);
        }
        let request = ApiRequest::get(&["api", "v1", "local-nodes", id, suffix]);
        self.execute(if suffix == "events" {
            request.query("after_sequence", 0).query("limit", 100)
        } else {
            request
        })
        .await
    }
    pub async fn local_node_events_after(
        &self,
        id: &str,
        sequence: u64,
    ) -> Result<Value, ApiError> {
        self.execute(
            ApiRequest::get(&["api", "v1", "local-nodes", id, "events"])
                .query("after_sequence", sequence)
                .query("limit", 100),
        )
        .await
    }
    pub async fn create_pairing_challenge(&self) -> Result<Value, ApiError> {
        self.execute(ApiRequest::mutation(
            Method::POST,
            &["api", "v1", "local-nodes", "pairing", "challenges"],
            Some(json!({"expires_in_seconds":180})),
        ))
        .await
    }
    pub async fn revoke_local_grant(&self, device: &str, grant: &str) -> Result<Value, ApiError> {
        self.execute(ApiRequest::mutation(
            Method::DELETE,
            &["api", "v1", "local-nodes", device, "grants", grant],
            None,
        ))
        .await
    }
    pub async fn session_history(&self, id: &str) -> Result<Value, ApiError> {
        self.execute(ApiRequest::get(&["api", "v1", "sessions", id, "history"]))
            .await
    }
    pub async fn update_session(&self, id: &str, body: Value) -> Result<Value, ApiError> {
        self.execute(ApiRequest::mutation(
            Method::PATCH,
            &["api", "v1", "sessions", id],
            Some(body),
        ))
        .await
    }
    pub async fn session_tools(&self, id: &str) -> Result<Value, ApiError> {
        self.execute(ApiRequest::get(&[
            "api",
            "v1",
            "assistant",
            "sessions",
            id,
            "tools",
        ]))
        .await
    }
    pub async fn set_memory_enabled(&self, enabled: bool) -> Result<Value, ApiError> {
        self.execute(ApiRequest::mutation(
            Method::PATCH,
            &["api", "v1", "assistant", "memory"],
            Some(json!({"enabled":enabled})),
        ))
        .await
    }
    pub async fn update_memory_item(&self, key: &str, value: &str) -> Result<Value, ApiError> {
        self.execute(ApiRequest::mutation(
            Method::PUT,
            &["api", "v1", "assistant", "memory", "items"],
            Some(json!({"key":key,"value":value})),
        ))
        .await
    }
    pub async fn delete_memory_item(&self, key: &str) -> Result<Value, ApiError> {
        self.execute(
            ApiRequest::mutation(
                Method::DELETE,
                &["api", "v1", "assistant", "memory", "items"],
                None,
            )
            .query("key", key),
        )
        .await
    }
    pub async fn skill_enabled(&self, name: &str, enabled: bool) -> Result<Value, ApiError> {
        self.execute(ApiRequest::mutation(
            Method::POST,
            &[
                "api",
                "v1",
                "skills",
                name,
                if enabled { "enable" } else { "disable" },
            ],
            None,
        ))
        .await
    }
    pub async fn list_documents(
        &self,
        dataset: &str,
        page: u32,
        limit: u32,
    ) -> Result<Value, ApiError> {
        self.execute(
            ApiRequest::get(&["api", "v1", "knowledge", dataset, "documents"])
                .query("page", page)
                .query("limit", limit),
        )
        .await
    }
    pub async fn document_action(
        &self,
        dataset: &str,
        document: &str,
        operation: &str,
    ) -> Result<Value, ApiError> {
        if !["reindex", "reprocess", "recover", "retry"].contains(&operation) {
            return Err(ApiError::Request);
        }
        self.execute(ApiRequest::mutation(
            Method::POST,
            &[
                "api",
                "v1",
                "knowledge",
                dataset,
                "documents",
                document,
                operation,
            ],
            None,
        ))
        .await
    }
    pub async fn retrieve(&self, dataset: &str, body: Value) -> Result<Value, ApiError> {
        self.execute(ApiRequest::mutation(
            Method::POST,
            &["api", "v1", "knowledge", dataset, "retrieve"],
            Some(body),
        ))
        .await
    }
    pub async fn agent_draft(&self, agent: &str) -> Result<Value, ApiError> {
        self.execute(ApiRequest::get(&["api", "v1", "agents", agent, "draft"]))
            .await
    }
    pub async fn update_agent_draft(&self, agent: &str, body: Value) -> Result<Value, ApiError> {
        self.execute(ApiRequest::mutation(
            Method::PUT,
            &["api", "v1", "agents", agent, "draft"],
            Some(body),
        ))
        .await
    }
    pub async fn task_result(&self, task: &str) -> Result<Value, ApiError> {
        self.execute(ApiRequest::get(&["api", "v1", "tasks", task, "result"]))
            .await
    }
    pub async fn request_trace(&self, id: &str) -> Result<Value, ApiError> {
        self.execute(ApiRequest::get(&["api", "v1", "usage", "traces", id]))
            .await
    }
    /// User-selected local bytes only; this does not read arbitrary host paths.
    pub async fn upload(
        &self,
        segments: &[&str],
        field: &str,
        filename: &str,
        mime: &str,
        bytes: Vec<u8>,
    ) -> Result<Value, ApiError> {
        self.upload_fields(segments, field, filename, mime, bytes, &[])
            .await
    }
    pub async fn upload_fields(
        &self,
        segments: &[&str],
        field: &str,
        filename: &str,
        mime: &str,
        bytes: Vec<u8>,
        fields: &[(&str, &str)],
    ) -> Result<Value, ApiError> {
        if bytes.len() > 100 * 1024 * 1024
            || filename.is_empty()
            || filename.contains(['\r', '\n', '"', '\\', '/', '\0'])
            || field.is_empty()
            || !field
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || mime.contains(['\r', '\n'])
        {
            return Err(ApiError::Request);
        }
        let boundary = format!(
            "platform-native-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| ApiError::Request)?
                .as_nanos()
        );
        let mut body=format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\nContent-Type: {mime}\r\n\r\n").into_bytes();
        body.extend(bytes);
        body.extend_from_slice(b"\r\n");
        for (name, value) in fields {
            if name.is_empty()
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || value.contains(['\0', '\r', '\n'])
            {
                return Err(ApiError::Request);
            }
            body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        let url = self.endpoint(&segments.iter().map(|s| (*s).into()).collect::<Vec<_>>())?;
        let mut response = self
            .http
            .post(url)
            .bearer_auth(self.token.as_ref())
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .map_err(|_| ApiError::Transport)?;
        if !response.status().is_success() {
            return Err(ApiError::Status(response.status().as_u16()));
        }
        let mut result = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| ApiError::Transport)? {
            if result.len() + chunk.len() > 8 * 1024 * 1024 {
                return Err(ApiError::Response);
            }
            result.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&result).map_err(|_| ApiError::Response)
    }
}

pub struct DownloadedBlob {
    pub bytes: Vec<u8>,
    pub mime: String,
}
impl DesktopApi {
    /// Bearer authentication stays on the configured origin. Untrusted absolute
    /// artifact links are handed to the user's browser, never fetched with a token.
    pub async fn download_blob(&self, reference: &str) -> Result<DownloadedBlob, ApiError> {
        if reference.starts_with("//") || reference.contains(['\\', '\r', '\n', '\0']) {
            return Err(ApiError::Request);
        }
        let url = self.origin.join(reference).map_err(|_| ApiError::Request)?;
        if url.origin() != self.origin.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(ApiError::Request);
        }
        let path = url.path();
        if !(path.starts_with("/api/v1/assistant/artifacts/") && path.ends_with("/download")
            || path.starts_with("/uploads/"))
        {
            return Err(ApiError::Request);
        }
        let mut response = self
            .http
            .get(url)
            .bearer_auth(self.token.as_ref())
            .send()
            .await
            .map_err(|_| ApiError::Transport)?;
        if !response.status().is_success() {
            return Err(ApiError::Status(response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|n| n > 100 * 1024 * 1024)
        {
            return Err(ApiError::Response);
        }
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .split(';')
            .next()
            .unwrap_or("application/octet-stream")
            .to_owned();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| ApiError::Transport)? {
            if bytes.len() + chunk.len() > 100 * 1024 * 1024 {
                return Err(ApiError::Response);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(DownloadedBlob { bytes, mime })
    }
    pub async fn execute_with_timeout(
        &self,
        request: ApiRequest,
        timeout: Duration,
    ) -> Result<Value, ApiError> {
        self.shared
            .execute_with_timeout(request, timeout)
            .await
            .map_err(ApiError::from)
    }
}
