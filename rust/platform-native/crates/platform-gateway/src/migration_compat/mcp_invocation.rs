//! Read-only MCP data plane with exact tenant/schema/grant authorization and pinned HTTP.
use super::{
    capability::{CapabilityAuthority, body_hash},
    common::{CompatError, header, text},
};
use crate::{
    mcp_gateway_broker::{McpClientCacheKey, McpSecretResolver, mcp_client_cache_key},
    mcp_http::{
        MCP_PROTOCOL_VERSION, McpClientConfig, McpDnsResolver, NativeMcpDnsResolver,
        decode_jsonrpc_response, select_pinned_address,
    },
};
use axum::{
    Json, Router,
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::post,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpReadRequest {
    pub connection_id: String,
    pub principal_type: String,
    #[serde(default = "default_channel")]
    pub channel: String,
    pub runtime_name: String,
    pub schema_hash: String,
    pub risk_level: String,
    #[serde(default = "empty_object")]
    pub arguments: Value,
    pub arguments_hash: String,
}
fn default_channel() -> String {
    "assistant".into()
}
fn empty_object() -> Value {
    json!({})
}
fn error(code: &str) -> CompatError {
    CompatError::new(502, code)
}
impl McpReadRequest {
    pub fn validate(&self) -> Result<(), CompatError> {
        let hash = |s: &str| {
            s.strip_prefix("sha256:").is_some_and(|v| {
                v.len() == 64
                    && v.bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            })
        };
        if self.connection_id.is_empty()
            || self.connection_id.len() > 255
            || self.runtime_name.is_empty()
            || self.runtime_name.len() > 255
            || self.channel.is_empty()
            || self.channel.len() > 64
            || !matches!(
                self.principal_type.as_str(),
                "service_account" | "user_delegated"
            )
            || !matches!(
                self.risk_level.as_str(),
                "low" | "medium" | "high" | "critical"
            )
            || !hash(&self.schema_hash)
            || !hash(&self.arguments_hash)
            || !self.arguments.is_object()
            || self.arguments_hash != format!("sha256:{}", body_hash(&self.arguments))
        {
            return Err(CompatError::new(422, "invalid MCP read request"));
        }
        Ok(())
    }
}
struct Session {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    credential: Option<String>,
    session: Mutex<Option<String>>,
    revision:crate::mcp_http::revision::Revision,
    oauth_resource:Option<String>,
    limit: usize,
    timeout: Duration,
    semaphore: Semaphore,
    circuit: Arc<super::mcp_circuit::McpCircuit>,
}
impl Session {
    async fn create(item: &Value, credential: Option<String>) -> Result<Self, CompatError> {
        let config = McpClientConfig::from_record(item, credential.clone())
            .map_err(|_| error("MCP_CONFIG_INVALID"))?;
        let mut endpoint =
            reqwest::Url::parse(&config.base_url).map_err(|_| error("MCP_URL_INVALID"))?;
        if endpoint.scheme() != "https" {
            return Err(error("MCP_TLS_REQUIRED"));
        }
        if !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(error("MCP_URL_INVALID"));
        }
        if config.auth_method == "oauth"
            && (config.oauth_resource.as_deref().unwrap_or("").is_empty()
                || config.oauth_audience.as_deref().unwrap_or("").is_empty()
                || config.oauth_audience != config.credential_audience)
        {
            return Err(error("MCP_OAUTH_AUDIENCE_MISMATCH"));
        }
        if config.revision.modern()&&config.auth_method=="oauth"&&config.oauth_resource!=config.oauth_audience{return Err(error("MCP_OAUTH_RESOURCE_MISMATCH"))}
        if !matches!(
            config.auth_method.as_str(),
            "none" | "bearer" | "oauth" | "api_key"
        ) {
            return Err(error("MCP_AUTH_METHOD_UNSUPPORTED"));
        }
        let host = endpoint
            .host_str()
            .ok_or_else(|| error("MCP_URL_INVALID"))?
            .trim_matches(['[', ']'])
            .to_owned();
        let port = endpoint
            .port_or_known_default()
            .ok_or_else(|| error("MCP_URL_INVALID"))?;
        let addresses = NativeMcpDnsResolver
            .resolve(&host, port)
            .await
            .map_err(|_| error("MCP_DNS_UNAVAILABLE"))?;
        let address = select_pinned_address(&addresses).map_err(|_| error("MCP_SSRF_BLOCKED"))?;
        if !config.endpoint_path.starts_with('/') || config.endpoint_path.contains("..") {
            return Err(error("MCP_URL_INVALID"));
        }
        let relative = config.endpoint_path.split('#').next().unwrap_or("");
        let (path, query) = relative
            .split_once('?')
            .map(|(p, q)| (p, Some(q)))
            .unwrap_or((relative, None));
        let path = format!(
            "{}/{}",
            endpoint.path().trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        endpoint.set_path(&path);
        endpoint.set_query(query);
        let client = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .resolve(&host, SocketAddr::new(address, port))
            .connect_timeout(config.timeout)
            .read_timeout(config.timeout)
            .build()
            .map_err(|_| error("MCP_UPSTREAM_UNAVAILABLE"))?;
        if config.auth_method=="oauth"&&(config.revision.modern()||config.oauth_metadata_url.is_some()){crate::mcp_http::oauth::fetch(&client,&endpoint,config.oauth_metadata_url.as_deref(),config.oauth_resource.as_deref().ok_or_else(||error("MCP_OAUTH_RESOURCE_MISMATCH"))?,config.timeout).await.map_err(error)?;}
        let session = Self {
            client,
            endpoint,
            credential,
            session: Mutex::new(None),
            revision:config.revision,
            oauth_resource:if config.auth_method=="oauth"{config.oauth_resource.clone()}else{None},
            limit: config.response_limit_bytes,
            timeout: config.timeout,
            semaphore: Semaphore::new(config.max_concurrency),
            circuit: super::mcp_circuit::McpCircuit::new(),
        };
        if session.revision.modern(){let result=session.rpc("server/discover",json!({})).await?;session.revision.validate_discovery(&result).map_err(error)?;}else{
        let initialized=session.rpc("initialize",json!({"protocolVersion":MCP_PROTOCOL_VERSION,"capabilities":{"tools":{"listChanged":true}},"clientInfo":{"name":"ai-gateway","version":"1.0.0"}})).await?;
        if text(&initialized, "protocolVersion") != MCP_PROTOCOL_VERSION {
            return Err(error("MCP_PROTOCOL_VERSION_MISMATCH"));
        }
        session
            .send(
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                true, Default::default(),
            )
            .await?;
        }
        Ok(session)
    }
    async fn send(
        &self,
        mut payload: Value,
        notification: bool,
        extra:reqwest::header::HeaderMap,
    ) -> Result<(String, Vec<u8>), CompatError> {
        let headers=self.revision.decorate(&mut payload).map_err(error)?;
        let mut request = self
            .client
            .post(self.endpoint.clone())
            .header("accept", "application/json, text/event-stream")
.headers(headers).headers(extra)
            .json(&payload);
        if let Some(credential) = &self.credential {
            request = request.bearer_auth(credential);
        }
        if let Some(session) = self
            .session
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone().filter(|_|!self.revision.modern())
        {
            request = request.header("mcp-session-id", session);
        }
        let mut response = request.send().await.map_err(|e| {
            error(if e.is_timeout() {
                "MCP_TIMEOUT"
            } else {
                "MCP_UPSTREAM_UNAVAILABLE"
            })
        })?;
        let status = response.status().as_u16();
        if status==401{if let Some(resource)=&self.oauth_resource{let challenge=response.headers().get("www-authenticate").and_then(|h|h.to_str().ok()).map(crate::mcp_http::oauth::challenge_url).transpose().map_err(error)?.flatten();crate::mcp_http::oauth::fetch(&self.client,&self.endpoint,challenge.as_deref(),resource,self.timeout).await.map_err(error)?;return Err(error("MCP_OAUTH_REAUTHORIZATION_REQUIRED"));}}
        if (300..400).contains(&status) {
            return Err(error("MCP_REDIRECT_BLOCKED"));
        }
        if status >= 500 {
            return Err(error("MCP_UPSTREAM_UNAVAILABLE"));
        }
        if (notification && !matches!(status, 200 | 202 | 204))
            || (!notification && !(200..300).contains(&status)&&!(self.revision.modern()&&status==400))
        {
            return Err(error("MCP_UPSTREAM_REJECTED"));
        }
        self.revision.check_session(response.headers()).map_err(error)?;
        if !self.revision.modern(){
        if response.headers().get_all("mcp-session-id").iter().count() > 1 {
            return Err(error("MCP_SESSION_INVALID"));
        }
        if let Some(received) = response.headers().get("mcp-session-id") {
            let received = received
                .to_str()
                .map_err(|_| error("MCP_SESSION_INVALID"))?;
            if received.is_empty()
                || received.len() > 256
                || !received.bytes().all(|v| (0x21..=0x7e).contains(&v))
            {
                return Err(error("MCP_SESSION_INVALID"));
            }
            let mut session = self.session.lock().unwrap_or_else(|p| p.into_inner());
            if session.as_deref().is_some_and(|old| old != received) {
                return Err(error("MCP_SESSION_CONFUSION_BLOCKED"));
            }
            *session = Some(received.into());
        }
        }
        if response
            .content_length()
            .is_some_and(|v| v > self.limit as u64)
        {
            return Err(error("MCP_RESPONSE_TOO_LARGE"));
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| error("MCP_UPSTREAM_UNAVAILABLE"))?
        {
            if bytes.len() + chunk.len() > self.limit {
                return Err(error("MCP_RESPONSE_TOO_LARGE"));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((content_type, bytes))
    }
    async fn rpc(&self,method:&str,params:Value)->Result<Value,CompatError>{self.rpc_headers(method,params,Default::default()).await}
    async fn rpc_headers(&self, method: &str, params: Value,extra:reqwest::header::HeaderMap) -> Result<Value, CompatError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (content_type, bytes) = self
            .send(
                json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
                false,extra,
            )
            .await?;
        if self.revision.modern(){let parsed=crate::mcp_http::decode_response_payload(&content_type,&bytes).map_err(|_|error("MCP_RESPONSE_INVALID"))?;if parsed["jsonrpc"]!="2.0"||parsed["id"].as_str()!=Some(id.as_str()){return Err(error("MCP_RESPONSE_INVALID"))}if let Some(code)=crate::mcp_http::revision::classify_error(&parsed){return Err(error(code))}}
        let result=decode_jsonrpc_response(&content_type,&bytes,&id).map_err(|e|match e{crate::mcp_registry_store::McpError::Broker(code)=>error(&code),_=>error("MCP_RESPONSE_INVALID")})?;
        self.revision.validate_result(&result).map_err(error)?;Ok(result)
    }

    async fn call_tool(&self, name: &str, arguments: &Value,schema:&Value) -> Result<Value, CompatError> {
        let mut circuit = None;
        let result = tokio::time::timeout(self.timeout, async {
            let _permit = self
                .semaphore
                .acquire()
                .await
                .map_err(|_| error("MCP_NOT_CONNECTED"))?;
            circuit = Some(self.circuit.acquire()?);
            let headers=if self.revision.modern(){crate::mcp_http::revision::tool_headers(schema,arguments).map_err(error)?}else{Default::default()};
            let mut result = self
                .rpc_headers("tools/call", json!({"name":name,"arguments":arguments}),headers)
                .await?;
            let content = result
                .get("content")
                .filter(|v| crate::service_registry::python_truthy(v))
                .cloned()
                .unwrap_or(json!([]));
            let content = content
                .as_array()
                .ok_or_else(|| error("MCP_RESPONSE_INVALID"))?;
            result["content"] = json!(
                content
                    .iter()
                    .filter(|v| v.is_object())
                    .cloned()
                    .collect::<Vec<_>>()
            );
            Ok(result)
        })
        .await
        .unwrap_or_else(|_| Err(error("MCP_HOST_DEADLINE")));
        if let Some(circuit) = circuit {
            circuit.finish(&result);
        }
        result
    }
}
struct CacheEntry {
    created: Instant,
    last_used: Instant,
    client: Arc<Session>,
}
struct Flight {
    sender: tokio::sync::watch::Sender<Option<Result<Arc<Session>, CompatError>>>,
    abort: tokio::task::AbortHandle,
}
#[derive(Default)]
struct ClientCache {
    entries: HashMap<McpClientCacheKey, CacheEntry>,
    inflight: HashMap<McpClientCacheKey, Flight>,
}
pub struct McpInvocationBroker {
    pool: PgPool,
    secrets: Arc<dyn McpSecretResolver>,
    cache: Arc<AsyncMutex<ClientCache>>,
    ttl: Duration,
    max_entries: usize,
}
impl McpInvocationBroker {
    pub fn new(pool: PgPool, secrets: Arc<dyn McpSecretResolver>) -> Self {
        Self {
            pool,
            secrets,
            cache: Arc::new(AsyncMutex::new(ClientCache::default())),
            ttl: Duration::from_secs(60),
            max_entries: 100,
        }
    }
    async fn acquire_client(
        &self,
        item: &Value,
        tenant: &str,
    ) -> Result<Arc<Session>, CompatError> {
        let key = mcp_client_cache_key(item, tenant).map_err(|_| error("MCP_CONFIG_INVALID"))?;
        let mut cache = self.cache.lock().await;
        cache.entries.retain(|cached, entry| {
            entry.created.elapsed() < self.ttl
                && (!(cached.tenant_id == key.tenant_id
                    && cached.connection_id == key.connection_id)
                    || cached == &key)
        });
        if let Some(entry) = cache.entries.get_mut(&key) {
            entry.last_used = Instant::now();
            return Ok(entry.client.clone());
        }
        let mut receiver = if let Some(flight) = cache.inflight.get(&key) {
            flight.sender.subscribe()
        } else {
            let (sender, receiver) = tokio::sync::watch::channel(None);
            let state = self.cache.clone();
            let secrets = self.secrets.clone();
            let item = item.clone();
            let task_key = key.clone();
            let limit = self.max_entries;
            let sender2 = sender.clone();
            let task = tokio::spawn(async move {
                let result = async {
                    let credential = if text(&item, "auth_method") == "none" {
                        None
                    } else {
                        let value = secrets
                            .resolve(text(&item, "secret_ref"))
                            .await
                            .map_err(|_| error("MCP_SECRET_UNAVAILABLE"))?;
                        if value.is_empty() {
                            return Err(error("MCP_SECRET_UNAVAILABLE"));
                        }
                        Some(value)
                    };
                    Ok(Arc::new(Session::create(&item, credential).await?))
                }
                .await;
                let mut cache = state.lock().await;
                if !cache.inflight.contains_key(&task_key) {
                    let _ = sender2.send(Some(Err(error("MCP_CLIENT_INVALIDATED"))));
                    return;
                }
                if let Ok(client) = &result {
                    cache.entries.insert(
                        task_key.clone(),
                        CacheEntry {
                            created: Instant::now(),
                            last_used: Instant::now(),
                            client: client.clone(),
                        },
                    );
                    while cache.entries.len() > limit {
                        if let Some(old) = cache
                            .entries
                            .iter()
                            .min_by_key(|(_, v)| v.last_used)
                            .map(|(k, _)| k.clone())
                        {
                            cache.entries.remove(&old);
                        } else {
                            break;
                        }
                    }
                }
                cache.inflight.remove(&task_key);
                let _ = sender2.send(Some(result));
            });
            cache.inflight.insert(
                key.clone(),
                Flight {
                    sender,
                    abort: task.abort_handle(),
                },
            );
            receiver
        };
        drop(cache);
        loop {
            let result = receiver.borrow().clone();
            if let Some(result) = result {
                let client = result?;
                let mut cache = self.cache.lock().await;
                let valid = cache
                    .entries
                    .get_mut(&key)
                    .filter(|entry| Arc::ptr_eq(&entry.client, &client));
                let Some(entry) = valid else {
                    return Err(error("MCP_CLIENT_INVALIDATED"));
                };
                entry.last_used = Instant::now();
                return Ok(client);
            }
            receiver
                .changed()
                .await
                .map_err(|_| error("MCP_CLIENT_INVALIDATED"))?;
        }
    }
    async fn authorize(
        &self,
        tenant: &str,
        user: &str,
        payload: &McpReadRequest,
    ) -> Result<Value, CompatError> {
        let deny = |code: &str| CompatError::new(403, code);
        if !matches!(
            payload.channel.as_str(),
            "preview" | "hosted_private" | "hosted_public" | "embed" | "api"
        ) {
            return Err(deny("MCP_PRINCIPAL_POLICY_DENIED"));
        }
        if uuid::Uuid::parse_str(&payload.connection_id).is_err() {
            return Err(deny("MCP_CAPABILITY_UNAVAILABLE"));
        }
        let item=sqlx::query_scalar::<_,Value>(r#"SELECT to_jsonb(bound) FROM (SELECT t.tool_id,t.server_id,t.upstream_name,t.runtime_name,s.schema_version,s.schema_hash,s.description,s.input_schema,s.risk_level,s.read_only,srv.base_url,srv.transport,srv.auth_method,srv.oauth_metadata_url,srv.oauth_resource,srv.oauth_audience,srv.allowed_origins,srv.timeout_ms,srv.max_concurrency,srv.response_limit_bytes,srv.health_status,srv.circuit_state,srv.circuit_open_until,srv.updated_at AS server_updated_at,c.connection_id,c.principal_type,c.owner_user_id,c.secret_ref,c.scopes,c.audience,c.expires_at,c.updated_at AS credential_updated_at,(srv.circuit_state='open' AND (srv.circuit_open_until IS NULL OR srv.circuit_open_until>NOW())) AS circuit_blocked FROM mcp_tools t JOIN mcp_tool_snapshots s ON s.tenant_id=t.tenant_id AND s.snapshot_id=t.current_snapshot_id JOIN mcp_servers srv ON srv.tenant_id=t.tenant_id AND srv.server_id=t.server_id JOIN mcp_connections c ON c.tenant_id=t.tenant_id AND c.server_id=t.server_id WHERE t.tenant_id=$1 AND t.runtime_name=$2 AND s.schema_hash=$3 AND c.connection_id=$4::uuid AND c.principal_type=$5 AND t.enabled=TRUE AND srv.enabled=TRUE AND srv.deleted_at IS NULL AND c.enabled=TRUE AND c.revoked_at IS NULL AND (c.expires_at IS NULL OR c.expires_at>NOW())) bound"#).bind(tenant).bind(&payload.runtime_name).bind(payload.schema_hash.trim_start_matches("sha256:")).bind(&payload.connection_id).bind(&payload.principal_type).fetch_optional(&self.pool).await?.ok_or_else(||deny("MCP_CAPABILITY_UNAVAILABLE"))?;
        if text(&item, "risk_level") != payload.risk_level {
            return Err(deny("MCP_RISK_CHANGED"));
        }
        if item["circuit_blocked"] == json!(true) {
            return Err(deny("MCP_CIRCUIT_OPEN"));
        }
        if text(&item, "auth_method") == "oauth"
            && (text(&item, "oauth_audience").is_empty()
                || item["audience"] != item["oauth_audience"])
        {
            return Err(deny("MCP_OAUTH_AUDIENCE_MISMATCH"));
        }
        let public = matches!(payload.channel.as_str(), "hosted_public" | "embed");
        if payload.principal_type == "user_delegated" {
            if user.is_empty() || text(&item, "owner_user_id") != user {
                return Err(deny("MCP_DELEGATED_PRINCIPAL_DENIED"));
            }
            if public {
                return Err(deny("MCP_PUBLIC_CHANNEL_DENIED"));
            }
        }
        let mut item = item;
        if public {
            let approved=sqlx::query_scalar::<_,bool>("SELECT read_only_only FROM mcp_channel_grants WHERE tenant_id=$1 AND connection_id=$2::uuid AND tool_id=$3::uuid AND channel=$4 AND enabled=TRUE AND approved_schema_hash=$5").bind(tenant).bind(&payload.connection_id).bind(text(&item,"tool_id")).bind(&payload.channel).bind(text(&item,"schema_hash")).fetch_optional(&self.pool).await?;
            if approved != Some(true) {
                return Err(deny("MCP_PUBLIC_CHANNEL_DENIED"));
            }
            item["read_only"] = json!(true);
        }
        if item["read_only"] != json!(true) {
            return Err(deny("MCP_WRITE_REQUIRES_CAPABILITY_EXECUTION"));
        }
        Ok(item)
    }
    pub async fn invoke_read_only(
        &self,
        tenant: &str,
        user: &str,
        payload: &McpReadRequest,
    ) -> Result<Value, CompatError> {
        let item = self.authorize(tenant, user, payload).await?;
        let client = self.acquire_client(&item, tenant).await?;
        let upstream = text(&item, "upstream_name");
        let result = client
            .call_tool(
                if upstream.is_empty() {
                    payload.runtime_name.as_str()
                } else {
                    upstream
                },
                &payload.arguments,&item["input_schema"],
            )
            .await?;
        Ok(
            json!({"tool_name":payload.runtime_name,"content":result.get("content").cloned().unwrap_or(json!([])),"is_error":result.get("isError").and_then(Value::as_bool).unwrap_or(false),"schema_hash":item["schema_hash"],"connection_id":item["connection_id"]}),
        )
    }
    pub async fn invalidate_connection(&self, tenant: &str, connection: &str) {
        let mut cache = self.cache.lock().await;
        cache
            .entries
            .retain(|key, _| key.tenant_id != tenant || key.connection_id != connection);
        let keys = cache
            .inflight
            .keys()
            .filter(|key| key.tenant_id == tenant && key.connection_id == connection)
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            if let Some(flight) = cache.inflight.remove(&key) {
                let _ = flight
                    .sender
                    .send(Some(Err(error("MCP_CLIENT_INVALIDATED"))));
                flight.abort.abort();
            }
        }
    }
    pub async fn close(&self) {
        let mut cache = self.cache.lock().await;
        cache.entries.clear();
        for (_, flight) in cache.inflight.drain() {
            let _ = flight
                .sender
                .send(Some(Err(error("MCP_CLIENT_INVALIDATED"))));
            flight.abort.abort();
        }
    }
}
#[derive(Clone)]
struct HttpState {
    authority: Arc<CapabilityAuthority>,
    broker: Arc<McpInvocationBroker>,
}
pub fn router(authority: Arc<CapabilityAuthority>, broker: Arc<McpInvocationBroker>) -> Router {
    Router::new()
        .route("/internal/v2/agent-capabilities/mcp/read", post(invoke))
        .layer(axum::middleware::from_fn(crate::migration_content::capabilities::scope_authenticated_workload))
        .with_state(HttpState { authority, broker })
}
async fn invoke(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let result = async {
        let payload: McpReadRequest = serde_json::from_value(body.clone())
            .map_err(|_| CompatError::new(422, "invalid MCP read request"))?;
        payload.validate()?;
        let scope =
            state
                .authority
                .verify(&headers, "/internal/v2/agent-capabilities/mcp/read", &body)?;
        state
            .authority
            .active_execution(
                &scope,
                &payload.runtime_name,
                "read",
                "not_required",
                &payload.arguments_hash,
            )
            .await?;
        state
            .broker
            .invoke_read_only(&scope.tenant_id, &scope.user_id, &payload)
            .await
    };
    match result.await {
        Ok(result) => Json(result).into_response(),
        Err(error) => error.into_response(),
    }
}
