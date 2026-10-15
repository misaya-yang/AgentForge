//! Authenticated native transport. Tokens never appear in Debug or error messages.
pub mod assistant_runs;
pub mod compatibility;
pub mod events;
pub mod management;
pub mod knowledge_sources;
pub mod knowledge_connections;
pub mod request;
pub mod scheduled_notifications;
pub mod scheduled_tasks;
pub mod sdk_services;
pub use request::ApiRequest;
pub mod sse;
use serde::Deserialize;
use serde_json::Value;
use std::{net::IpAddr, sync::Arc, time::Duration};
use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum Error {
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
#[derive(Clone)]
pub struct Client {
    origin: Url,
    http: reqwest::Client,
    token: Option<Arc<str>>,
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
        })
    }
    pub fn authenticated(&self, token: String) -> Self {
        Self {
            token: Some(token.into()),
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
    async fn get(&self, segments: &[&str]) -> Result<Value, Error> {
        let token = self.token.as_ref().ok_or(Error::Authentication)?;
        self.decode(
            self.http
                .get(self.endpoint(segments)?)
                .bearer_auth(token.as_ref()),
        )
        .await
    }
    async fn post(&self, segments: &[&str], body: Value) -> Result<Value, Error> {
        let token = self.token.as_ref().ok_or(Error::Authentication)?;
        self.decode(
            self.http
                .post(self.endpoint(segments)?)
                .bearer_auth(token.as_ref())
                .json(&body),
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
