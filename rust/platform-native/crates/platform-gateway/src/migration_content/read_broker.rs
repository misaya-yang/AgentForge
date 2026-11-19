//! Private bounded web search and complete-redacted tool-output reads.
use super::{
    ContentError, ContentFuture,
    artifacts::ArtifactStorage,
    capabilities::{CapabilityAuthorization, CapabilityScope},
    sha256, string,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::HeaderMap,
    routing::post,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactReadRequest {
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "default_limit")]
    pub limit: usize,
}
fn default_limit() -> usize {
    64000
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebSearchRequest {
    pub queries: Vec<String>,
    #[serde(default = "default_results")]
    pub max_results: usize,
}
fn default_results() -> usize {
    5
}
#[derive(Clone)]
pub struct TavilySearch {
    client: reqwest::Client,
    api_key: Arc<str>,
}
impl TavilySearch {
    pub fn new(api_key: String) -> Result<Self, ContentError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .read_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .pool_max_idle_per_host(5)
            .build()
            .map_err(|_| unavailable())?;
        Ok(Self {
            client,
            api_key: api_key.trim().to_owned().into(),
        })
    }
    pub fn with_client(api_key: String, client: reqwest::Client) -> Self {
        Self {
            client,
            api_key: api_key.trim().to_owned().into(),
        }
    }
    async fn one(&self, query: String, max: usize) -> Result<Value, ContentError> {
        let mut response=self.client.post("https://api.tavily.com/search").bearer_auth(self.api_key.as_ref()).json(&json!({"query":query,"max_results":max,"search_depth":"basic","include_answer":false,"include_raw_content":false,"include_images":false})).send().await.map_err(|_|unavailable())?;
        if response.status().as_u16() >= 400 {
            return Err(unavailable());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
            if chunk.len() > (2 * 1024 * 1024usize).saturating_sub(body.len()) {
                return Err(unavailable());
            }
            body.extend_from_slice(&chunk)
        }
        let payload: Value = serde_json::from_slice(&body).map_err(|_| unavailable())?;
        let items = payload["results"].as_array().ok_or_else(unavailable)?;
        let results = items
            .iter()
            .take(max)
            .filter_map(bounded_search_result)
            .collect::<Vec<_>>();
        Ok(json!({"query":query,"results":results}))
    }
    pub async fn search(&self, request: WebSearchRequest) -> Result<Value, ContentError> {
        if self.api_key.is_empty() {
            return Err(unavailable());
        }
        let mut tasks = tokio::task::JoinSet::new();
        let count = request.queries.len();
        for (index, query) in request.queries.into_iter().enumerate() {
            let this = self.clone();
            tasks.spawn(async move {
                this.one(query, request.max_results)
                    .await
                    .map(|result| (index, result))
            });
        }
        let mut results = vec![Value::Null; count];
        while let Some(task) = tasks.join_next().await {
            let (index, result) = task.map_err(|_| unavailable())??;
            results[index] = result;
        }
        Ok(
            json!({"schema_version":"agent-web-search-result/v1","provider":"tavily","queries":results}),
        )
    }
}
fn unavailable() -> ContentError {
    ContentError::new(503, "web search unavailable")
}
pub fn bounded_search_result(value: &Value) -> Option<Value> {
    let (title, url, content) = (
        value["title"].as_str()?,
        value["url"].as_str()?,
        value["content"].as_str()?,
    );
    let parsed = reqwest::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || url.chars().count() > 4096
    {
        return None;
    }
    let score = value["score"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
    Some(
        json!({"title":title.chars().take(500).collect::<String>(),"url":url,"content":content.chars().take(4000).collect::<String>(),"score":score}),
    )
}
pub struct ContentReadBroker {
    pub artifacts: Arc<ArtifactStorage>,
    pub authorization: CapabilityAuthorization,
    pub search: Arc<TavilySearch>,
}
impl ContentReadBroker {
    pub async fn read(
        &self,
        scope: &CapabilityScope,
        id: &str,
        request: &ArtifactReadRequest,
    ) -> Result<Value, ContentError> {
        let missing = || ContentError::new(404, "artifact not found");
        if !id.strip_prefix("art_").is_some_and(|s| {
            (8..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
        }) {
            return Err(missing());
        }
        let (artifact, raw) = self
            .artifacts
            .read_scoped(
                id,
                &scope.tenant_id,
                &scope.session_id,
                &scope.user_id,
                2_000_000,
                "tool_output_spill",
            )
            .await
            .map_err(|_| missing())?
            .ok_or_else(missing)?;
        let meta = &artifact.metadata;
        if artifact.source != "tool_output_spill"
            || meta["schema_version"] != "assistant-tool-output-artifact/v1"
            || meta["redacted"] != true
            || meta["complete_redacted"] != true
            || meta["content_kind"] != "text"
            || raw.len() > 2_000_000
        {
            return Err(missing());
        }
        let text = std::str::from_utf8(&raw).map_err(|_| missing())?;
        let total = text.chars().count();
        let digest = sha256(&raw);
        let receipt = string(meta, "host_receipt_id");
        if meta["content_sha256"] != digest
            || meta["content_chars"].as_u64() != Some(total as u64)
            || receipt.is_empty()
            || artifact.turn_id.as_deref() != Some(receipt)
        {
            return Err(missing());
        }
        if request.offset > total {
            return Err(ContentError::new(416, "offset out of range"));
        }
        let content = text
            .chars()
            .skip(request.offset)
            .take(request.limit)
            .collect::<String>();
        let end = request.offset + content.chars().count();
        let next = if end < total { Some(end) } else { None };
        Ok(
            json!({"artifact_id":id,"content":content,"offset":request.offset,"next_offset":next,"total_chars":total,"content_sha256":digest,"complete":next.is_none(),"artifact_complete":true,"redacted":true,"redaction_receipt":{"schema_version":"assistant-tool-output-artifact/v1","complete_redacted":true,"host_verified":true}}),
        )
    }
}
pub fn router(broker: Arc<ContentReadBroker>) -> Router {
    Router::new()
        .route("/internal/v2/agent-capabilities/web-search", post(search))
        .route(
            "/internal/v2/agent-capabilities/artifacts/{id}/read",
            post(read),
        )
        .layer(axum::middleware::from_fn(super::capabilities::scope_authenticated_workload))
        .with_state(broker)
}
async fn search(
    State(b): State<Arc<ContentReadBroker>>,
    headers: HeaderMap,
    Json(mut request): Json<WebSearchRequest>,
) -> Result<Json<Value>, ContentError> {
    if !(1..=5).contains(&request.queries.len()) || !(1..=10).contains(&request.max_results) {
        return Err(ContentError::new(422, "Invalid web search request"));
    }
    let body = serde_json::to_value(&request)?;
    b.authorization.authorize(
        &headers,
        "/internal/v2/agent-capabilities/web-search",
        &body,
        chrono::Utc::now().timestamp(),
    )?;
    for query in &mut request.queries {
        *query = query.trim().to_owned();
        if query.is_empty() || query.chars().count() > 500 || query.chars().any(|c| (c as u32) < 32)
        {
            return Err(ContentError::new(422, "web search query is invalid"));
        }
    }
    Ok(Json(b.search.search(request).await?))
}
async fn read(
    State(b): State<Arc<ContentReadBroker>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ArtifactReadRequest>,
) -> Result<Json<Value>, ContentError> {
    if request.offset > 2_000_000 || !(1..=2_000_000).contains(&request.limit) {
        return Err(ContentError::new(422, "Invalid artifact read request"));
    }
    let body = serde_json::to_value(&request)?;
    let scope = b.authorization.authorize(
        &headers,
        &format!("/internal/v2/agent-capabilities/artifacts/{id}/read"),
        &body,
        chrono::Utc::now().timestamp(),
    )?;
    Ok(Json(b.read(&scope, &id, &request).await?))
}
