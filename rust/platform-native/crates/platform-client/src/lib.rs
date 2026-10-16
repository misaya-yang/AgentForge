//! Authenticated native transport. Tokens never appear in Debug or error messages.
pub mod assistant_runs;
pub mod compatibility;
pub mod events;
pub mod knowledge_connections;
pub mod knowledge_sources;
pub mod management;
pub mod request;
pub mod scheduled_notifications;
pub mod scheduled_tasks;
pub mod sdk_services;
pub use request::ApiRequest;
pub mod sdk;
pub mod sdk_models;
pub mod sdk_stream;
pub mod sdk_threads;
pub mod sse;
pub use sdk::AssistantClient;
pub mod assistant_sdk;
pub mod mcp;
use serde::Deserialize;
use serde_json::Value;
use std::{net::IpAddr, sync::Arc, time::Duration};
use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Submission outcome unknown; do not automatically resubmit")]
    UnknownOutcome,
    #[error("Use an HTTPS origin, or an explicit loopback IP over HTTP")]
    Origin,
    #[error("Invalid request")]
    Request,
    #[error("Request failed")]
    Transport,
    #[error("Server returned HTTP {0}")]
    Status(u16),
    #[error("Invalid or oversized server response")]
    Response,
    #[error("{0}")]
    Contract(&'static str),
    #[error("Authentication required")]
    Authentication,
}
/// Per-stream header and idle limits; independent from ordinary HTTP request timeouts.
/// Values are positive and capped at 24 hours. Cancellation and consumer drop
/// always remain active, even with long-running reasoning model limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamLimits {
    header_timeout: Duration,
    idle_timeout: Duration,
}
impl StreamLimits {
    pub fn new(header_timeout: Duration, idle_timeout: Duration) -> Result<Self, Error> {
        let maximum = Duration::from_secs(24 * 60 * 60);
        if header_timeout.is_zero()
            || idle_timeout.is_zero()
            || header_timeout > maximum
            || idle_timeout > maximum
        {
            return Err(Error::Request);
        }
        Ok(Self {
            header_timeout,
            idle_timeout,
        })
    }
    pub fn header_timeout(self) -> Duration {
        self.header_timeout
    }
    pub fn idle_timeout(self) -> Duration {
        self.idle_timeout
    }
}
impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            header_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(90),
        }
    }
}
#[derive(Clone)]
pub struct Client {
    origin: Url,
    http: reqwest::Client,
    token: Option<Arc<str>>,
    sdk_auth: Option<Arc<SdkAuth>>,
    default_timeout: Duration,
    read_retries: u8,
    stream_limits: StreamLimits,
}
struct SdkAuth {
    api_key: String,
    tenant_id: String,
    user_id: Option<String>,
}
#[derive(Deserialize)]
pub struct Login {
    pub access_token: String,
    pub force_password_change: bool,
}
impl Client {
    pub fn new(origin: &str) -> Result<Self, Error> {
        let origin = Url::parse(origin).map_err(|_| Error::Origin)?;
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
            return Err(Error::Origin);
        }
        let http = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| Error::Transport)?;
        Ok(Self {
            origin,
            http,
            token: None,
            sdk_auth: None,
            default_timeout: Duration::from_secs(30),
            stream_limits: StreamLimits::default(),
            read_retries: 0,
        })
    }
    pub fn with_stream_limits(mut self, limits: StreamLimits) -> Self {
        self.stream_limits = limits;
        self
    }
    pub fn stream_limits(&self) -> StreamLimits {
        self.stream_limits
    }
    pub fn authenticated(&self, token: String) -> Self {
        Self {
            token: Some(token.into()),
            sdk_auth: None,
            read_retries: 0,
            ..self.clone()
        }
    }
    fn endpoint(&self, segments: &[&str]) -> Result<Url, Error> {
        if segments
            .iter()
            .any(|s| s.is_empty() || *s == "." || *s == ".." || s.contains(['/', '\\', '\r', '\n']))
        {
            return Err(Error::Response);
        }
        let mut url = self.origin.clone();
        url.path_segments_mut()
            .map_err(|_| Error::Origin)?
            .clear()
            .extend(segments);
        Ok(url)
    }
    async fn decode(&self, request: reqwest::RequestBuilder) -> Result<Value, Error> {
        let mut response = request.send().await.map_err(|_| Error::Transport)?;
        if !response.status().is_success() {
            return Err(Error::Status(response.status().as_u16()));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Transport)? {
            if bytes.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
                return Err(Error::Response);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| Error::Response)
    }
    pub async fn login(&self, email: String, password: String) -> Result<Login, Error> {
        let request = self
            .http
            .post(self.endpoint(&["api", "v1", "auth", "login"])?)
            .json(&serde_json::json!({"email":email,"password":password}));
        let result = self.decode(request).await?;
        let login: Login = serde_json::from_value(result).map_err(|_| Error::Response)?;
        if login.access_token.is_empty() {
            return Err(Error::Response);
        }
        Ok(login)
    }
    pub fn authenticated_api_key(
        &self,
        api_key: String,
        tenant_id: String,
        user_id: Option<String>,
    ) -> Result<Self, Error> {
        if api_key.is_empty()
            || [&api_key, &tenant_id]
                .into_iter()
                .chain(user_id.as_ref())
                .any(|s| s.contains(['\r', '\n', '\0']))
        {
            return Err(Error::Authentication);
        }
        Ok(Self {
            token: None,
            read_retries: 3,
            sdk_auth: Some(Arc::new(SdkAuth {
                api_key,
                tenant_id,
                user_id: user_id.filter(|s| !s.is_empty()),
            })),
            ..self.clone()
        })
    }
    pub fn with_transport_options(
        &self,
        timeout: Duration,
        read_retries: u8,
    ) -> Result<Self, Error> {
        if timeout.is_zero() {
            return Err(Error::Request);
        }
        Ok(Self {
            default_timeout: timeout,
            read_retries,
            ..self.clone()
        })
    }
    async fn send_request(
        &self,
        request: reqwest::RequestBuilder,
        read_only: bool,
    ) -> Result<reqwest::Response, Error> {
        let retries = if read_only { self.read_retries } else { 0 };
        let mut attempt = 0u16;
        loop {
            let pending = request.try_clone().ok_or(Error::Request)?;
            let result = pending.send().await;
            let retry = match &result {
                Ok(r) => [429, 502, 503, 504].contains(&r.status().as_u16()),
                Err(_) => true,
            };
            if !retry || attempt >= retries as u16 {
                return result.map_err(|_| Error::Transport);
            }
            let backoff = (0.5 * 2f64.powi(attempt.min(10) as i32)).min(30.);
            let seconds = result
                .as_ref()
                .ok()
                .and_then(|r| r.headers().get("Retry-After"))
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v > 0.)
                .map(|v| v.min(30.))
                .unwrap_or(backoff);
            drop(result);
            tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
            attempt += 1;
        }
    }
    fn authorize(
        &self,
        mut request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, Error> {
        if let Some(auth) = &self.sdk_auth {
            request = request
                .header("X-API-Key", &auth.api_key)
                .header("X-Tenant-Id", &auth.tenant_id)
                .header("X-SDK-Version", "rust/1.0.1")
                .header("X-Request-Id", uuid::Uuid::new_v4().simple().to_string());
            if let Some(user) = &auth.user_id {
                request = request.header("X-User-Id", user);
            }
            Ok(request)
        } else {
            let token = self
                .token
                .as_ref()
                .filter(|t| !t.is_empty() && !t.contains(['\r', '\n', '\0']))
                .ok_or(Error::Authentication)?;
            Ok(request.bearer_auth(token.as_ref()))
        }
    }
    async fn get(&self, segments: &[&str]) -> Result<Value, Error> {
        self.endpoint(segments)?;
        self.execute(ApiRequest::get(segments)).await
    }
    async fn post(&self, segments: &[&str], body: Value) -> Result<Value, Error> {
        self.decode(
            self.authorize(
                self.http
                    .post(self.endpoint(segments)?)
                    .timeout(self.default_timeout)
                    .json(&body),
            )?,
        )
        .await
    }
    pub async fn me(&self) -> Result<Value, Error> {
        self.get(&["api", "v1", "auth", "me"]).await
    }
    /// Single attempt; transport failure can leave the password change outcome unknown.
    pub async fn change_password(
        &self,
        current: String,
        new: String,
        confirmation: String,
    ) -> Result<(), Error> {
        self.post(&["api", "v1", "auth", "change-password"], serde_json::json!({"current_password":current,"new_password":new,"confirm_password":confirmation})).await.and_then(|body| if body.get("status").and_then(Value::as_str) == Some("success") { Ok(()) } else { Err(Error::Response) })
    }
    pub async fn logout(&self) -> Result<(), Error> {
        self.post(&["api", "v1", "auth", "logout"], serde_json::json!({}))
            .await
            .map(|_| ())
    }
    pub async fn create_thread(
        &self,
        session: Option<&str>,
        model: Option<&str>,
    ) -> Result<Value, Error> {
        let mut body = session.map_or_else(
            || serde_json::json!({}),
            |id| serde_json::json!({"session_id":id}),
        );
        if let Some(model) = model {
            body["model_id"] = Value::String(model.into());
        }
        self.post(&["api", "v2", "agent", "threads"], body).await
    }
    /// Single attempt only: an uncertain POST must never be automatically retried.
    pub async fn create_turn(
        &self,
        thread: &str,
        message: &str,
        model: Option<&str>,
    ) -> Result<Value, Error> {
        if message.is_empty() || message.chars().count() > 200_000 {
            return Err(Error::Response);
        }
        let mut body =
            serde_json::json!({"message":message,"execution_profile":"safe","kb_mode":"off"});
        if let Some(model) = model {
            body["model_id"] = Value::String(model.into());
        }
        self.post(&["api", "v2", "agent", "threads", thread, "turns"], body)
            .await
    }
    pub async fn interrupt(&self, thread: &str, turn: &str) -> Result<Value, Error> {
        self.post(
            &[
                "api",
                "v2",
                "agent",
                "threads",
                thread,
                "turns",
                &format!("{turn}:interrupt"),
            ],
            serde_json::json!({"reason":"client_interrupt"}),
        )
        .await
    }
    /// Only same-origin returned event routes for the exact thread may receive the token.
    pub fn events_url(&self, thread: &str, relative: &str) -> Result<Url, Error> {
        if !relative.starts_with('/') || relative.starts_with("//") || relative.contains('\\') {
            return Err(Error::Response);
        }
        let expected = self.endpoint(&["api", "v2", "agent", "threads", thread, "events"])?;
        let result = self.origin.join(relative).map_err(|_| Error::Response)?;
        if result.origin() != self.origin.origin()
            || result.path() != expected.path()
            || result.fragment().is_some()
        {
            return Err(Error::Response);
        }
        Ok(result)
    }
    pub async fn models(&self) -> Result<Value, Error> {
        self.get(&["api", "v1", "assistant", "models"]).await
    }
    pub async fn datasets(&self) -> Result<Value, Error> {
        self.get(&["api", "v1", "assistant", "datasets"]).await
    }
    pub async fn sessions(&self) -> Result<Value, Error> {
        self.get(&["api", "v1", "assistant", "sessions"]).await
    }
    pub async fn history(&self, id: &str) -> Result<Value, Error> {
        self.get(&["api", "v1", "assistant", "sessions", id, "history"])
            .await
    }
}
