//! Stateless MCP 2026-07-28. No session, initialization, subscriptions or token
//! forwarding to third parties. Calls dispatch the established REST handlers.
use super::config::McpConfiguration;
use ai_platform_auth::admission::AdmissionService;
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
pub const VERSION: &str = "2026-07-28";
#[derive(Clone)]
struct Server {
    configuration: McpConfiguration,
    rest: Router,
    admission: Arc<AdmissionService>,
    published_agents: bool,
    external_agents: Vec<String>,
    audit_pool: sqlx::PgPool,
}
pub fn router(
    configuration: McpConfiguration,
    rest: Router,
    admission: Arc<AdmissionService>,
    published_agents: bool,
    external_agents: Vec<String>,
    audit_pool: sqlx::PgPool,
) -> Router {
    Router::new()
        .route("/mcp", post(dispatch))
        .route("/.well-known/oauth-protected-resource/mcp", get(metadata))
        .with_state(Server {
            configuration,
            rest,
            admission,
            published_agents,
            external_agents,
            audit_pool,
        })
}
async fn metadata(State(s): State<Server>) -> Response {
    ( [(header::CACHE_CONTROL,"public, max-age=300")],Json(json!({"resource":s.configuration.resource,"authorization_servers":s.configuration.authorization_servers,"bearer_methods_supported":["header"],"scopes_supported":["knowledge:read","agents:read","admin:read"]}))).into_response()
}
fn unauthorized(s: &Server, status: StatusCode) -> Response {
    let mut response = (
        status,
        Json(json!({"detail":"Audience-bound OAuth access token required"})),
    )
        .into_response();
    if status == StatusCode::UNAUTHORIZED {
        let origin = s.configuration.resource.trim_end_matches("/mcp");
        if let Ok(value) = format!(
            "Bearer resource_metadata=\"{origin}/.well-known/oauth-protected-resource/mcp\""
        )
        .parse()
        {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, value);
        }
    }
    response
}
/// This only narrows a token already verified by AdmissionService. Decoding
/// without verifying first must never be used as an authentication boundary.
fn audience(headers: &HeaderMap, expected: &str) -> bool {
    let Some(raw) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return false;
    };
    let parts = raw.split('.').collect::<Vec<_>>();
    if parts.len() != 3 || parts[1].len() > 32768 {
        return false;
    }
    let Ok(raw) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[1]) else {
        return false;
    };
    let Ok(claims) = serde_json::from_slice::<Value>(&raw) else {
        return false;
    };
    claims["aud"].as_str() == Some(expected)
        || claims["aud"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(expected)))
}
fn rpc(id: Value, mut result: Value) -> Response {
    result["_meta"] = json!({"io.modelcontextprotocol/serverInfo":{"name":"AgentForge Gateway","version":"1.0.0"}});
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"jsonrpc":"2.0","id":id,"result":result})),
    )
        .into_response()
}
fn error(id: Value, code: i32, message: &str) -> Response {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})),
    )
        .into_response()
}
fn schema(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn tools() -> Vec<Value> {
    vec![
        json!({"name":"knowledge_retrieve","description":"Retrieve authorized dataset evidence through the Knowledge service.","inputSchema":schema(json!({"dataset_id":{"type":"string","format":"uuid"},"query":{"type":"string","minLength":1,"maxLength":16000},"top_k":{"type":"integer","minimum":1,"maximum":50}}),&["dataset_id","query"]),"annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}}),
        json!({"name":"agents_list","description":"List Agents visible to the current authenticated tenant and user.","inputSchema":schema(json!({}),&[]),"annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true}}),
        json!({"name":"agent_publications","description":"Read immutable publication/version metadata for an authorized Agent.","inputSchema":schema(json!({"agent_id":{"type":"string","format":"uuid"}}),&["agent_id"]),"annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true}}),
        json!({"name":"published_agent_send","description":"Send text to an immutable published Agent. Reuse message_id after a lost response; never invent a replacement ID for an unknown effect.","inputSchema":schema(json!({"publication_id":{"type":"string","format":"uuid"},"message_id":{"type":"string","format":"uuid"},"context_id":{"type":"string","format":"uuid"},"text":{"type":"string","minLength":1,"maxLength":16000}}),&["publication_id","message_id","text"]),"annotations":{"readOnlyHint":false,"destructiveHint":true,"idempotentHint":false,"openWorldHint":true}}),
        json!({"name":"external_agent_send","description":"Invoke a registered external Agent through current tenant tool policy and a pre-debited durable budget. Unknown effects must be polled or reviewed; never resubmit with a replacement message ID.","inputSchema":schema(json!({"agent_name":{"type":"string","pattern":"^[A-Za-z0-9_-]{1,64}$"},"message_id":{"type":"string","format":"uuid"},"text":{"type":"string","minLength":1,"maxLength":16000}}),&["agent_name","message_id","text"]),"annotations":{"readOnlyHint":false,"destructiveHint":true,"idempotentHint":false,"openWorldHint":true}}),
        json!({"name":"external_agent_task","description":"Read a current external task using the original local message ID; unknown sends are reconciled by a read-only message lookup.","inputSchema":schema(json!({"agent_name":{"type":"string","pattern":"^[A-Za-z0-9_-]{1,64}$"},"message_id":{"type":"string","format":"uuid"}}),&["agent_name","message_id"]),"annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":true}}),
        json!({"name":"external_agent_cancel","description":"Request cancellation once. Unknown acknowledgements require task polling; do not repeat with a replacement ID.","inputSchema":schema(json!({"agent_name":{"type":"string","pattern":"^[A-Za-z0-9_-]{1,64}$"},"message_id":{"type":"string","format":"uuid"}}),&["agent_name","message_id"]),"annotations":{"readOnlyHint":false,"destructiveHint":true,"idempotentHint":false,"openWorldHint":true}}),
        json!({"name":"external_agent_reconcile","description":"Resolve an ambiguous external send using only the verified peer message lookup. Never sends a new task.","inputSchema":schema(json!({"agent_name":{"type":"string","pattern":"^[A-Za-z0-9_-]{1,64}$"},"message_id":{"type":"string","format":"uuid"}}),&["agent_name","message_id"]),"annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":true}}),
        json!({"name":"admin_models","description":"Read the registered model catalog using the existing administrative REST policy.","inputSchema":schema(json!({}),&[]),"annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true}}),
    ]
}
fn tool_list(published: bool, external: &[String]) -> Vec<Value> {
    tools()
        .into_iter()
        .filter(|t| {
            (published || t["name"] != "published_agent_send")
                && (!external.is_empty()
                    || !t["name"]
                        .as_str()
                        .is_some_and(|name| name.starts_with("external_agent_")))
        })
        .map(|mut tool| {
            if tool["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("external_agent_"))
            {
                tool["inputSchema"]["properties"]["agent_name"]["enum"] = json!(external);
            }
            tool
        })
        .collect()
}
fn target(name: &str, args: &Value) -> Result<(&'static str, String, Value), &'static str> {
    for key in [
        "dataset_id",
        "agent_id",
        "publication_id",
        "message_id",
        "context_id",
    ] {
        if let Some(id) = args.get(key) {
            if !id
                .as_str()
                .is_some_and(|v| uuid::Uuid::parse_str(v).is_ok())
            {
                return Err("Invalid resource ID");
            }
        }
    }
    let Some(tool) = tools().into_iter().find(|t| t["name"] == name) else {
        return Err("Unknown tool");
    };
    let validator =
        jsonschema::validator_for(&tool["inputSchema"]).map_err(|_| "Tool schema unavailable")?;
    if !validator.is_valid(args) {
        return Err("Invalid tool arguments");
    }
    match name {
        "knowledge_retrieve" => Ok((
            "POST",
            format!(
                "/api/v1/knowledge/{}/retrieve",
                args["dataset_id"].as_str().unwrap()
            ),
            json!({"query":args["query"],"top_k":args.get("top_k").cloned().unwrap_or(json!(5))}),
        )),
        "agents_list" => Ok(("GET", "/api/v1/agents".into(), Value::Null)),
        "agent_publications" => Ok((
            "GET",
            format!(
                "/api/v1/agents/{}/publications",
                args["agent_id"].as_str().unwrap()
            ),
            Value::Null,
        )),
        "published_agent_send" => {
            let mut message = json!({"messageId":args["message_id"],"role":"ROLE_USER","parts":[{"text":args["text"]}]});
            if let Some(context) = args.get("context_id") {
                message["contextId"] = context.clone();
            }
            Ok((
                "POST",
                format!(
                    "/a2a/{}/message:send",
                    args["publication_id"].as_str().unwrap()
                ),
                json!({"message":message,"configuration":{"returnImmediately":true}}),
            ))
        }
        "external_agent_send" => Ok((
            "POST",
            format!(
                "/a2a/outbound/{}/message:send",
                args["agent_name"].as_str().unwrap()
            ),
            json!({"message":{"messageId":args["message_id"],"role":"ROLE_USER","parts":[{"text":args["text"]}]},"configuration":{"returnImmediately":true}}),
        )),
        "external_agent_task" | "external_agent_cancel" | "external_agent_reconcile" => Ok((
            if name == "external_agent_task" {
                "GET"
            } else {
                "POST"
            },
            format!(
                "/a2a/outbound/{}/tasks/{}{}",
                args["agent_name"].as_str().unwrap(),
                args["message_id"].as_str().unwrap(),
                match name {
                    "external_agent_cancel" => ":cancel",
                    "external_agent_reconcile" => ":reconcile",
                    _ => "",
                }
            ),
            if name == "external_agent_task" {
                Value::Null
            } else {
                json!({})
            },
        )),
        "admin_models" => Ok(("GET", "/api/v1/models".into(), Value::Null)),
        _ => Err("Unknown tool"),
    }
}
async fn invoke(
    s: &Server,
    headers: &HeaderMap,
    name: &str,
    args: &Value,
) -> Result<(Value, u16), &'static str> {
    if name == "published_agent_send" && !s.published_agents {
        return Err("Published Agent invocation is unavailable");
    }
    if name.starts_with("external_agent_")
        && !args["agent_name"]
            .as_str()
            .is_some_and(|name| s.external_agents.iter().any(|n| n == name))
    {
        return Err("External Agent is unavailable");
    }
    delegate(s.rest.clone(), headers, name, args).await
}
async fn delegate(
    rest: Router,
    headers: &HeaderMap,
    name: &str,
    args: &Value,
) -> Result<(Value, u16), &'static str> {
    let (method, path, body) = target(name, args)?;
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    for key in ["authorization", "x-request-id", "traceparent"] {
        if let Some(v) = headers.get(key) {
            request = request.header(key, v);
        }
    }
    let request = request
        .body(if body.is_null() {
            Body::empty()
        } else {
            Body::from(body.to_string())
        })
        .map_err(|_| "Invalid tool request")?;
    let response = rest
        .oneshot(request)
        .await
        .map_err(|_| "Tool unavailable")?;
    let status = response.status();
    let raw = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .map_err(|_| "Tool response too large")?;
    let value = serde_json::from_slice::<Value>(&raw).unwrap_or_else(
        |_| json!({"detail":"Tool returned a non-JSON response","status":status.as_u16()}),
    );
    Ok((
        json!({"resultType":"complete","isError":!status.is_success(),"structuredContent":value,"content":[{"type":"text","text":value.to_string()}]}),
        status.as_u16(),
    ))
}
async fn dispatch(State(s): State<Server>, request: Request) -> Response {
    let headers = request.headers().clone();
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if !s.configuration.allowed_origins.iter().any(|o| o == origin) {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    if !headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(';').next() == Some("application/json"))
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let admission = s.admission.request(&headers, "/mcp");
    let (audit_tenant, audit_user) = match admission.resolve().await {
        Ok(p) if p.jwt_principal().is_some() => (p.tenant_id().to_owned(), p.user_id().to_owned()),
        Ok(_) => return unauthorized(&s, StatusCode::UNAUTHORIZED),
        Err(e) => {
            return unauthorized(
                &s,
                StatusCode::from_u16(e.status()).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
            );
        }
    };
    if !audience(&headers, &s.configuration.resource) {
        return unauthorized(&s, StatusCode::UNAUTHORIZED);
    }
    let bytes = match to_bytes(request.into_body(), 1024 * 1024).await {
        Ok(v) => v,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return error(Value::Null, -32700, "Parse error"),
    };
    let notification = value.is_object()
        && value.get("id").is_none()
        && value["jsonrpc"] == "2.0"
        && value["method"].is_string();
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    if value["jsonrpc"] != "2.0"
        || !notification && !(id.is_string() || id.as_i64().is_some())
        || !value["params"].is_object()
    {
        return error(id, -32600, "Invalid request");
    }
    let method = value["method"].as_str().unwrap_or("");
    if headers.get("mcp-method").and_then(|v| v.to_str().ok()) != Some(method) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if method == "tools/call"
        && headers.get("mcp-name").and_then(|v| v.to_str().ok()) != value["params"]["name"].as_str()
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let versions = &value["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"];
    if versions.as_str() != Some(VERSION) {
        return (StatusCode::BAD_REQUEST,Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32022,"message":"Unsupported protocol version","data":{"requested":versions.as_str().unwrap_or(""),"supported":[VERSION]}}}))).into_response();
    }
    if headers
        .get("mcp-protocol-version")
        .and_then(|v| v.to_str().ok())
        != Some(VERSION)
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if !value["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"].is_object() {
        return error(id, -32602, "Per-request client capabilities required");
    }
    if notification {
        return StatusCode::ACCEPTED.into_response();
    }
    match method {
        "server/discover" => rpc(
            id,
            json!({"resultType":"complete","supportedVersions":[VERSION],"capabilities":{"tools":{}},"cacheScope":"private","ttlMs":0}),
        ),
        "tools/list" => rpc(
            id,
            json!({"resultType":"complete","tools":tool_list(s.published_agents,&s.external_agents),"cacheScope":"private","ttlMs":0}),
        ),
        "tools/call" => {
            let name = value["params"]["name"].as_str().unwrap_or("");
            let invoked = invoke(&s, &headers, name, &value["params"]["arguments"]).await;
            let request_id = headers
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .filter(|s| uuid::Uuid::parse_str(s).is_ok())
                .map(str::to_owned)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let status = invoked.as_ref().map(|(_, status)| *status).unwrap_or(400);
            let receipt = ai_platform_gateway_foundation::external_a2a_ledger::Receipt {
                kind: ai_platform_gateway_foundation::external_a2a_ledger::Kind::Mcp,
                request_id,
                client_request_id: None,
                request_sha256: None,
                wire_response_id: None,
                tool: Some(
                    if tools().iter().any(|t| t["name"] == name) {
                        name
                    } else {
                        "unknown_tool"
                    }
                    .into(),
                ),
                local_message_id: None,
                remote_task_id: None,
                outcome: if (200..300).contains(&status) {
                    "completed"
                } else {
                    "denied"
                }
                .into(),
                http_status: status,
            };
            if ai_platform_gateway_foundation::external_a2a_ledger::record(
                &s.audit_pool,
                &audit_tenant,
                &audit_user,
                &receipt,
            )
            .await
            .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            match invoked {
                Ok((result, _)) => rpc(id, result),
                Err(message) => error(id, -32602, message),
            }
        }
        _ => error(id, -32601, "Method not found"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_typed_routes_do_not_accept_path_or_identity_injection() {
        assert!(
            target(
                "knowledge_retrieve",
                &json!({"dataset_id":"../../internal","query":"q"})
            )
            .is_err()
        );
        assert!(target("agents_list", &json!({"tenant_id":"other"})).is_err());
        assert!(target("admin_models", &json!({"method":"DELETE"})).is_err());
        assert!(target("unknown", &json!({})).is_err());
    }
    #[test]
    fn audience_requires_exact_resource() {
        let mut h = HeaderMap::new();
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(json!({"aud":["gateway","https://example.test/mcp"]}).to_string());
        h.insert(
            header::AUTHORIZATION,
            format!("Bearer e30.{payload}.sig").parse().unwrap(),
        );
        assert!(audience(&h, "https://example.test/mcp"));
        assert!(!audience(&h, "https://evil.test/mcp"));
    }
    #[tokio::test]
    async fn delegate_preserves_rest_denial_and_never_forwards_identity_headers() {
        let rest = Router::new().route(
            "/api/v1/agents",
            get(|headers: HeaderMap| async move {
                assert!(headers.get("x-user-id").is_none());
                assert_eq!(headers.get(header::AUTHORIZATION).unwrap(), "Bearer test");
                (StatusCode::FORBIDDEN, Json(json!({"detail":"denied"})))
            }),
        );
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer test".parse().unwrap());
        headers.insert("x-user-id", "forged".parse().unwrap());
        headers.insert("x-tenant-id", "forged".parse().unwrap());
        let result = delegate(rest, &headers, "agents_list", &json!({}))
            .await
            .unwrap();
        assert_eq!(result.0["isError"], true);
        assert_eq!(result.0["structuredContent"]["detail"], "denied");
    }
}

#[cfg(test)]
mod schema_tests {
    use super::*;
    #[test]
    fn generated_tools_and_stateless_results_match_the_official_pinned_schema() {
        let schema: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/protocols/mcp-2026-07-28.json"
        ))
        .unwrap();
        for (definition, value) in [
            (
                "ListToolsResult",
                json!({"resultType":"complete","tools":tools(),"cacheScope":"private","ttlMs":0}),
            ),
            (
                "DiscoverResult",
                json!({"resultType":"complete","supportedVersions":[VERSION],"capabilities":{"tools":{}},"cacheScope":"private","ttlMs":0}),
            ),
            (
                "CallToolResult",
                json!({"resultType":"complete","isError":false,"structuredContent":{"ok":true},"content":[{"type":"text","text":"ok"}]}),
            ),
        ] {
            let mut selected = schema.clone();
            selected["$ref"] = json!(format!("#/$defs/{definition}"));
            let validator = jsonschema::validator_for(&selected).unwrap();
            assert!(validator.is_valid(&value), "{definition}");
        }
    }
    #[tokio::test]
    async fn invalid_arguments_cannot_reach_a_rest_handler() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = count.clone();
        let rest = Router::new().fallback(move || {
            let count = observed.clone();
            async move {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                StatusCode::OK
            }
        });
        assert!(
            delegate(
                rest,
                &HeaderMap::new(),
                "knowledge_retrieve",
                &json!({"dataset_id":"../admin","query":"x"})
            )
            .await
            .is_err()
        );
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}

#[cfg(test)]
mod verified_token_tests {
    use super::*;
    #[test]
    fn resource_audience_is_narrowed_only_after_real_signature_verification() {
        let resource = "https://gateway.example/mcp";
        let secret = b"a-native-test-secret-of-at-least-32-bytes";
        let codec = ai_platform_auth::TokenCodec::new(
            secret,
            Some(resource.into()),
            Some("https://gateway.example".into()),
        )
        .unwrap();
        let now = 1_800_000_000;
        let claims = ai_platform_auth::Claims {
            sub: "user".into(),
            user_id: "user".into(),
            tenant_id: "tenant".into(),
            email: None,
            roles: vec![],
            permissions: vec![],
            tier: "normal".into(),
            exp: now + 300,
            iat: now,
            nbf: None,
            jti: uuid::Uuid::new_v4().to_string(),
            aud: Some(ai_platform_auth::Audience::One(resource.into())),
            iss: Some("https://gateway.example".into()),
        };
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(secret),
        )
        .unwrap();
        assert!(codec.verify(&token, now).is_ok());
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        assert!(audience(&headers, resource));
        assert!(!audience(&headers, "https://other.example/mcp"));
        let wrong_codec = ai_platform_auth::TokenCodec::new(
            b"a-different-native-secret-of-at-least-32-bytes",
            Some(resource.into()),
            Some("https://gateway.example".into()),
        )
        .unwrap();
        assert!(wrong_codec.verify(&token, now).is_err());
    }
}

#[cfg(test)]
mod external_registration_tests {
    use super::*;
    #[test]
    fn external_tools_publish_only_configured_peer_names_and_share_the_governed_rest_target() {
        assert!(
            !tool_list(false, &[])
                .iter()
                .any(|t| t["name"] == "external_agent_send")
        );
        let listed = tool_list(true, &["peer".into()]);
        let tool = listed
            .iter()
            .find(|t| t["name"] == "external_agent_send")
            .unwrap();
        assert_eq!(
            tool["inputSchema"]["properties"]["agent_name"]["enum"],
            json!(["peer"])
        );
        let(_,path,body)=target("external_agent_send",&json!({"agent_name":"peer","message_id":"00000000-0000-4000-8000-000000000001","text":"x"})).unwrap();
        assert_eq!(path, "/a2a/outbound/peer/message:send");
        assert_eq!(body["configuration"]["returnImmediately"], true);
        for (name, suffix, method) in [
            ("external_agent_task", "", "GET"),
            ("external_agent_cancel", ":cancel", "POST"),
            ("external_agent_reconcile", ":reconcile", "POST"),
        ] {
            let (actual, path, _) = target(
                name,
                &json!({"agent_name":"peer","message_id":"00000000-0000-4000-8000-000000000001"}),
            )
            .unwrap();
            assert_eq!(actual, method);
            assert_eq!(
                path,
                format!("/a2a/outbound/peer/tasks/00000000-0000-4000-8000-000000000001{suffix}")
            );
        }

        assert!(target("external_agent_send",&json!({"agent_name":"../peer","message_id":"00000000-0000-4000-8000-000000000001","text":"x"})).is_err());
    }
}
