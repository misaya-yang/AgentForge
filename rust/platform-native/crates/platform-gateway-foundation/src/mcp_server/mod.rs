//! Platform MCP server: stateless 2026-07-28 and 2025-11-25 compatibility.
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use std::sync::Arc;

pub const CURRENT: &str = "2026-07-28";
pub const LEGACY: &str = "2025-11-25";

#[derive(Clone)]
pub struct McpConfig {
    pub knowledge_base: String,
    pub bearer: String,
    pub resource: String,
    pub http: reqwest::Client,
}

pub fn space_tools() -> Vec<Value> {
    vec![
        tool("space_list", "List knowledge spaces visible to the caller.", json!({}), &[], true),
        tool("space_search", "Search pages in knowledge spaces.", json!({"query":{"type":"string"},"space_id":{"type":"string"}}), &["query"], true),
        tool("space_read_page", "Read one knowledge space page.", json!({"item_id":{"type":"string"}}), &["item_id"], true),
        tool("space_create_page", "Create a page in a knowledge space. Requires approval.", json!({"space_id":{"type":"string"},"title":{"type":"string"}}), &["space_id","title"], false),
        tool("space_append_blocks", "Append markdown to a knowledge space page. Requires approval.", json!({"item_id":{"type":"string"},"markdown":{"type":"string"}}), &["item_id","markdown"], false),
        tool("knowledge_search", "Search authorized knowledge.", json!({"query":{"type":"string"},"dataset_id":{"type":"string"}}), &["query"], true),
    ]
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str], read_only: bool) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type":"object","properties":properties,"required":required,"additionalProperties":false},
        "annotations": {"readOnlyHint": read_only, "destructiveHint": !read_only, "idempotentHint": read_only, "openWorldHint": false}
    })
}

pub fn router(config: McpConfig) -> Router {
    Router::new()
        .route("/mcp", post(dispatch).get(stream_unsupported))
        .route("/.well-known/oauth-protected-resource/mcp", get(metadata))
        .with_state(Arc::new(config))
}

async fn stream_unsupported(State(_config): State<Arc<McpConfig>>) -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}

async fn metadata(State(config): State<Arc<McpConfig>>) -> Json<Value> {
    Json(json!({
        "resource": config.resource,
        "authorization_servers": [],
        "bearer_methods_supported": ["header"],
        "scopes_supported": ["connector:view", "knowledge:space:view"]
    }))
}

fn unauthorized(config: &McpConfig) -> Response {
    let origin = config.resource.trim_end_matches("/mcp");
    let mut response = (
        StatusCode::UNAUTHORIZED,
        Json(json!({"detail":"Bearer token required"})),
    )
        .into_response();
    if let Ok(value) = format!(
        "Bearer resource_metadata=\"{origin}/.well-known/oauth-protected-resource/mcp\""
    )
    .parse()
    {
        response.headers_mut().insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

async fn dispatch(State(config): State<Arc<McpConfig>>, headers: HeaderMap, body: String) -> Response {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if presented != Some(config.bearer.as_str()) || config.bearer.is_empty() {
        return unauthorized(&config);
    }
    let value: Value = match serde_json::from_str(&body) {
        Ok(value) => value,
        Err(_) => return rpc_error(Value::Null, -32700, "Parse error"),
    };
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let method = value["method"].as_str().unwrap_or("");
    let protocol = headers
        .get("mcp-protocol-version")
        .and_then(|value| value.to_str().ok())
        .unwrap_or(if method == "initialize" { LEGACY } else { "" });
    if protocol == LEGACY {
        return legacy(&config, id, method, &value).await;
    }
    if protocol != CURRENT {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if headers.get("mcp-method").and_then(|value| value.to_str().ok()) != Some(method) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if value["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"].as_str() != Some(CURRENT)
        || !value["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"].is_object()
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    respond(&config, id, method, &value).await
}

async fn legacy(config: &McpConfig, id: Value, method: &str, value: &Value) -> Response {
    if method == "initialize" {
        let mut response = rpc(
            id,
            json!({"protocolVersion": LEGACY, "capabilities": {"tools": {}}, "serverInfo": {"name": "agentforge", "version": "m3"}}),
        );
        response.headers_mut().insert(
            "mcp-session-id",
            "legacy".parse().unwrap(),
        );
        return response;
    }
    if method == "notifications/initialized" {
        return StatusCode::ACCEPTED.into_response();
    }
    respond(config, id, method, value).await
}

async fn respond(config: &McpConfig, id: Value, method: &str, value: &Value) -> Response {
    match method {
        "initialize" => rpc(
            id,
            json!({"protocolVersion": CURRENT, "capabilities": {"tools": {}}, "serverInfo": {"name": "agentforge", "version": "m3"}}),
        ),
        "server/discover" => rpc(
            id,
            json!({"resultType":"complete","supportedVersions":[CURRENT, LEGACY],"capabilities":{"tools":{}},"cacheScope":"private","ttlMs":0}),
        ),
        "tools/list" => rpc(
            id,
            json!({"resultType":"complete","tools":space_tools(),"cacheScope":"private","ttlMs":0}),
        ),
        "tools/call" => {
            let name = value["params"]["name"].as_str().unwrap_or("");
            let arguments = &value["params"]["arguments"];
            match call_space_tool(config, name, arguments).await {
                Ok(result) => rpc(id, result),
                Err(message) => rpc_error(id, -32602, message),
            }
        }
        _ => rpc_error(id, -32601, "Method not found"),
    }
}

pub async fn call_space_tool(config: &McpConfig, name: &str, arguments: &Value) -> Result<Value, &'static str> {
    let read_only = space_tools()
        .into_iter()
        .find(|tool| tool["name"] == name)
        .ok_or("Unknown tool")?["annotations"]["readOnlyHint"]
        == true;
    if !read_only {
        return Err("approval_required");
    }
    let base = config.knowledge_base.trim_end_matches('/');
    let (method, path, body) = match name {
        "space_list" => ("GET", "/api/v1/knowledge/spaces".to_owned(), Value::Null),
        "space_search" => {
            let query = arguments["query"].as_str().ok_or("Invalid tool arguments")?;
            let mut path = format!("/api/v1/knowledge/space-items?q={}", urlencoding(query));
            if let Some(space_id) = arguments["space_id"].as_str() {
                path.push_str(&format!("&space_id={}", urlencoding(space_id)));
            }
            ("GET", path, Value::Null)
        }
        "space_read_page" => {
            let item_id = arguments["item_id"].as_str().ok_or("Invalid tool arguments")?;
            ("GET", format!("/api/v1/knowledge/space-items/{item_id}/content"), Value::Null)
        }
        "knowledge_search" => {
            let query = arguments["query"].as_str().ok_or("Invalid tool arguments")?;
            ("GET", format!("/api/v1/knowledge/space-items?q={}", urlencoding(query)), Value::Null)
        }
        _ => return Err("Unknown tool"),
    };
    let url = format!("{base}{path}");
    let request = if method == "GET" {
        config.http.get(url)
    } else {
        config.http.post(url).json(&body)
    };
    let response = request.send().await.map_err(|_| "Tool unavailable")?;
    let status = response.status();
    let payload = response.json::<Value>().await.unwrap_or(Value::Null);
    Ok(json!({
        "resultType": "complete",
        "isError": !status.is_success(),
        "structuredContent": payload,
        "content": [{"type":"text","text":payload.to_string()}]
    }))
}

fn rpc(id: Value, result: Value) -> Response {
    (StatusCode::OK, Json(json!({"jsonrpc":"2.0","id":id,"result":result}))).into_response()
}

fn rpc_error(id: Value, code: i32, message: &str) -> Response {
    (
        StatusCode::OK,
        Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})),
    )
        .into_response()
}

fn urlencoding(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use tower::ServiceExt;

    fn config(base: &str) -> McpConfig {
        McpConfig {
            knowledge_base: base.into(),
            bearer: "test-token".into(),
            resource: "http://127.0.0.1:8096/mcp".into(),
            http: reqwest::Client::new(),
        }
    }

    async fn post(router: Router, token: Option<&str>, headers: &[(&str, &str)], body: Value) -> Response {
        let mut request = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        router
            .oneshot(request.body(axum::body::Body::from(body.to_string())).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn mcp_server_missing_token_is_401_with_resource_metadata() {
        let response = post(router(config("http://127.0.0.1:9")), None, &[], json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}})).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let header = response
            .headers()
            .get(axum::http::header::WWW_AUTHENTICATE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(header.contains("resource_metadata="));
        assert!(header.contains("/.well-known/oauth-protected-resource/mcp"));
    }

    #[tokio::test]
    async fn mcp_server_legacy_and_current_protocols_list_space_tools() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let app = Router::new().route(
                "/api/v1/knowledge/space-items",
                get(|| async { Json(json!({"items":[{"item_id":"page-1","title":"M2 page"}]})) }),
            );
            axum::serve(listener, app).await.unwrap();
        });
        let mcp = router(config(&format!("http://{address}")));
        let current = post(
            mcp.clone(),
            Some("test-token"),
            &[
                ("mcp-protocol-version", CURRENT),
                ("mcp-method", "tools/list"),
            ],
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":CURRENT,"io.modelcontextprotocol/clientCapabilities":{}}}}),
        )
        .await;
        let current_body: Value = serde_json::from_slice(&to_bytes(current.into_body(), 1024 * 1024).await.unwrap()).unwrap();
        assert!(current_body["result"]["tools"].as_array().unwrap().iter().any(|tool| tool["name"] == "space_search"));
        let legacy = post(
            mcp.clone(),
            Some("test-token"),
            &[("mcp-protocol-version", LEGACY)],
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
        )
        .await;
        let legacy_body: Value = serde_json::from_slice(&to_bytes(legacy.into_body(), 1024 * 1024).await.unwrap()).unwrap();
        assert!(legacy_body["result"]["tools"].as_array().unwrap().iter().any(|tool| tool["name"] == "space_search"));
        let called = post(
            mcp,
            Some("test-token"),
            &[("mcp-protocol-version", LEGACY)],
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"space_search","arguments":{"query":"M2"}}}),
        )
        .await;
        let called_body: Value = serde_json::from_slice(&to_bytes(called.into_body(), 1024 * 1024).await.unwrap()).unwrap();
        assert!(called_body["result"]["structuredContent"]["items"][0]["title"] == "M2 page");
    }
}
