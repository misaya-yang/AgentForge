//! Explicit, user-authorized ChatGPT plan adapter. Construction is inert: no
//! environment/file discovery, credential creation, browser launch or API-key fallback.
//! The host owns consent UI, loopback listener, account selection and protected storage.
mod loopback;
mod oauth;
mod store;
pub use loopback::LoopbackLogin;
pub use store::ProtectedStore;
mod stream;
pub use oauth::{Account, AccountIdentity, AuthorizationAttempt, SecretString};
pub use stream::{ResponseEvent, ResponseStream};

use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fmt, time::Duration};

pub const RESOURCE: &str = "https://api.openai.com/v1";
pub const DIRECT_SCOPE: &str = "chatgpt.tokens.use.direct";
const MAX_JSON: usize = 1024 * 1024;

/// Deliberately has no upstream body/credential-bearing Debug representation.
pub struct PlanError {
    pub kind: &'static str,
    pub status: Option<u16>,
    pub request_id: Option<String>,
    /// Sanitized diagnostic preserving upstream JSON shape. Never automatically logged.
    pub diagnostic: Option<Value>,
}
impl PlanError {
    pub(crate) fn local(kind: &'static str) -> Self {
        Self {
            kind,
            status: None,
            request_id: None,
            diagnostic: None,
        }
    }
    pub fn code(&self) -> Option<&str> {
        let v = self.diagnostic.as_ref()?;
        v.pointer("/error/code")
            .or_else(|| v.pointer("/response/error/code"))
            .or_else(|| v.get("error").filter(|x| x.is_string()))
            .and_then(Value::as_str)
    }
    pub fn requires_sign_in(&self) -> bool {
        matches!(
            self.code(),
            Some(
                "invalid_grant"
                    | "invalid_refresh_token"
                    | "token_expired"
                    | "refresh_token_expired"
                    | "refresh_token_invalidated"
                    | "refresh_token_reused"
            )
        )
    }
}
impl fmt::Debug for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlanError")
            .field("kind", &self.kind)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}
impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChatGPT plan: {}", self.kind)
    }
}
impl std::error::Error for PlanError {}
pub type Result<T> = std::result::Result<T, PlanError>;

#[derive(Clone)]
pub struct ChatGptPlanClient {
    pub(crate) http: Client,
}
impl ChatGptPlanClient {
    pub fn new() -> Result<Self> {
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(|_| PlanError::local("client_configuration"))?;
        Ok(Self { http })
    }
    /// Fetches the selected registration's current choices, preserving server order.
    pub async fn models(&self, account: &Account) -> Result<ModelCatalog> {
        let token = self.access_token(account).await?;
        let response = self
            .http
            .get(format!("{RESOURCE}/models"))
            .bearer_auth(token.expose())
            .send()
            .await
            .map_err(|_| PlanError::local("transport"))?;
        let value = checked_json(response, &[token.expose()]).await?;
        let parsed: CatalogWire =
            serde_json::from_value(value).map_err(|_| PlanError::local("invalid_catalog"))?;
        let models = parsed
            .models
            .into_iter()
            .filter(|m| m.visibility == "list" && !m.slug.is_empty())
            .collect();
        Ok(ModelCatalog {
            identity: account.identity.clone(),
            models,
        })
    }
    /// Small supported text/history surface. Model must come from this account's catalog.
    /// Requests are never automatically retried; a started stream may already consume usage.
    pub async fn responses(
        &self,
        account: &Account,
        catalog: &ModelCatalog,
        model: &str,
        input: Vec<TextMessage>,
        instructions: Option<&str>,
    ) -> Result<ResponseStream> {
        if catalog.identity != account.identity || !catalog.models.iter().any(|m| m.slug == model) {
            return Err(PlanError::local("model_not_available_for_account"));
        }
        if input.is_empty() {
            return Err(PlanError::local("input_required"));
        }
        let input = input.into_iter().map(ResponseInput::Message).collect();
        self.responses_with_tools(account, catalog, model, input, instructions, Vec::new())
            .await
    }
    /// Client-executed function tools use namespaces; hosted tools are deliberately excluded.
    pub async fn responses_with_tools(
        &self,
        account: &Account,
        catalog: &ModelCatalog,
        model: &str,
        input: Vec<ResponseInput>,
        instructions: Option<&str>,
        tools: Vec<FunctionNamespace>,
    ) -> Result<ResponseStream> {
        if catalog.identity != account.identity || !catalog.models.iter().any(|m| m.slug == model) {
            return Err(PlanError::local("model_not_available_for_account"));
        }
        if input.is_empty() {
            return Err(PlanError::local("input_required"));
        }
        for namespace in &tools {
            if namespace.name.is_empty()
                || namespace
                    .tools
                    .iter()
                    .any(|t| t.name.is_empty() || !t.parameters.is_object())
            {
                return Err(PlanError::local("invalid_function_tool"));
            }
        }
        let mut body = json!({"model": model, "input": input, "store": false, "stream": true});
        if !tools.is_empty() {
            body["tools"] = serde_json::to_value(tools)
                .map_err(|_| PlanError::local("invalid_function_tool"))?;
        }
        if let Some(text) = instructions {
            body["instructions"] = json!(text);
        }
        let token = self.access_token(account).await?;
        let response = self
            .http
            .post(format!("{RESOURCE}/responses"))
            .bearer_auth(token.expose())
            .header("Accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(|_| PlanError::local("transport"))?;
        if !response.status().is_success() {
            return Err(upstream_error(response, &[token.expose()]).await);
        }
        if !response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(';').next() == Some("text/event-stream"))
        {
            return Err(PlanError::local("expected_event_stream"));
        }
        Ok(ResponseStream::new(response, token))
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    Developer,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextMessage {
    pub role: MessageRole,
    pub content: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailableModel {
    pub slug: String,
    pub display_name: String,
    pub visibility: String,
}
#[derive(Deserialize)]
struct CatalogWire {
    models: Vec<AvailableModel>,
}
#[derive(Debug)]
pub struct ModelCatalog {
    identity: AccountIdentity,
    pub models: Vec<AvailableModel>,
}
impl ModelCatalog {
    /// A preference, never an assertion that Luna exists or is allowed for this user.
    pub fn preferred_luna(&self) -> Option<&AvailableModel> {
        self.models.iter().find(|m| {
            m.slug.to_ascii_lowercase().contains("luna")
                || m.display_name.to_ascii_lowercase().contains("luna")
        })
    }
}

pub(crate) async fn bounded_bytes(mut response: Response) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| PlanError::local("transport"))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_JSON {
            return Err(PlanError::local("response_too_large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
pub(crate) async fn checked_json(response: Response, secrets: &[&str]) -> Result<Value> {
    if !response.status().is_success() {
        return Err(upstream_error(response, secrets).await);
    }
    serde_json::from_slice(&bounded_bytes(response).await?)
        .map_err(|_| PlanError::local("invalid_json"))
}
pub(crate) async fn upstream_error(response: Response, secrets: &[&str]) -> PlanError {
    let status = Some(response.status().as_u16());
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| redact_string(s, secrets));
    let diagnostic = bounded_bytes(response).await.ok().map(|b| {
        let v = serde_json::from_slice(&b)
            .unwrap_or_else(|_| json!({"detail": String::from_utf8_lossy(&b)}));
        redact(v, secrets)
    });
    PlanError {
        kind: "upstream",
        status,
        request_id,
        diagnostic,
    }
}
fn redact_string(value: &str, secrets: &[&str]) -> String {
    let mut result = value.to_owned();
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        result = result.replace(secret, "[REDACTED]");
    }
    result
}
pub(crate) fn redact(value: Value, secrets: &[&str]) -> Value {
    match value {
        Value::String(s) => Value::String(redact_string(&s, secrets)),
        Value::Array(v) => Value::Array(v.into_iter().map(|v| redact(v, secrets)).collect()),
        Value::Object(v) => Value::Object(
            v.into_iter()
                .map(|(k, v)| {
                    let hidden = matches!(
                        k.as_str(),
                        "access_token"
                            | "refresh_token"
                            | "id_token"
                            | "code_verifier"
                            | "authorization"
                    );
                    (
                        k,
                        if hidden {
                            json!("[REDACTED]")
                        } else {
                            redact(v, secrets)
                        },
                    )
                })
                .collect(),
        ),
        v => v,
    }
}

/// Replay the complete history, including function calls and matching outputs.
#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseInput {
    Message(TextMessage),
    Tool(ToolHistoryItem),
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolHistoryItem {
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
}
#[derive(Clone, Serialize, Deserialize)]
pub struct FunctionNamespace {
    #[serde(rename = "type")]
    kind: NamespaceType,
    pub name: String,
    pub description: String,
    pub tools: Vec<FunctionTool>,
}
impl FunctionNamespace {
    pub fn new(name: String, description: String, tools: Vec<FunctionTool>) -> Self {
        Self {
            kind: NamespaceType::Namespace,
            name,
            description,
            tools,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum NamespaceType {
    Namespace,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct FunctionTool {
    #[serde(rename = "type")]
    kind: FunctionType,
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub strict: bool,
}
impl FunctionTool {
    pub fn new(name: String, description: String, parameters: Value, strict: bool) -> Self {
        Self {
            kind: FunctionType::Function,
            name,
            description,
            parameters,
            strict,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FunctionType {
    Function,
}
