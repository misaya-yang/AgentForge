//! Fixed internal bridge from Agent dynamic tool calls to the Gateway capability plane.

use std::net::IpAddr;

use codex_app_server_protocol::DynamicToolCallParams;
use codex_protocol::ThreadId;
use reqwest::Url;
use serde_json::Value;

use crate::CapabilityAllowlistEntry;
use crate::PlatformThreadIdentity;
use crate::trace_context::InternalTraceContext;

fn endpoint(base_url: &str) -> Result<Url, String> {
    let mut url = Url::parse(base_url).map_err(|_| "capability_plane_url_invalid".to_string())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("capability_plane_url_invalid".to_string());
    }
    if url.username() != ""
        || url.password().is_some()
        || url
            .host_str()
            .is_some_and(|host| host.parse::<IpAddr>().is_ok_and(|ip| ip.is_unspecified()))
    {
        return Err("capability_plane_url_invalid".to_string());
    }
    if !url.path().ends_with("/invoke") {
        let path = format!("{}/invoke", url.path().trim_end_matches('/'));
        url.set_path(&path);
    }
    Ok(url)
}

fn build_invoke_payload(
    params: &DynamicToolCallParams,
    identity: &PlatformThreadIdentity,
    capability_revision: i64,
    snapshot_id: &str,
    bound_dataset_ids: &[String],
    capability_allowlist: &[CapabilityAllowlistEntry],
    expected_tool: &CapabilityAllowlistEntry,
) -> Value {
    serde_json::json!({
        "tenant_id": identity.tenant_id,
        "user_id": identity.user_id,
        "session_id": identity.session_id,
        "capability_revision": capability_revision,
        "snapshot_id": snapshot_id,
        "run_id": params.turn_id,
        "tool": params.tool,
        "arguments": params.arguments,
        "bound_dataset_ids": bound_dataset_ids,
        "capability_allowlist": capability_allowlist,
        "expected_tool": expected_tool,
    })
}

/// Invoke exactly one dynamic read-only capability and return the Codex
/// DynamicToolCallResponse-shaped JSON. The model loop remains in Codex; this
/// helper only handles the capability-service hop and response projection.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn invoke_dynamic_tool(
    client: &reqwest::Client,
    params: &DynamicToolCallParams,
    identity: &PlatformThreadIdentity,
    capability_plane_url: &str,
    internal_token: &str,
    capability_revision: i64,
    snapshot_id: &str,
    bound_dataset_ids: &[String],
    capability_allowlist: &[CapabilityAllowlistEntry],
    expected_tool: &CapabilityAllowlistEntry,
    trace_context: &InternalTraceContext,
) -> Result<Value, String> {
    let url = endpoint(capability_plane_url)?;
    if internal_token.is_empty() {
        return Err("capability_plane_token_missing".to_string());
    }
    let thread_id = ThreadId::from_string(&params.thread_id)
        .map_err(|_| "capability_plane_thread_invalid".to_string())?;
    if thread_id != identity.runtime_thread_id {
        return Err("capability_plane_thread_scope_mismatch".to_string());
    }
    if capability_allowlist
        .iter()
        .filter(|entry| *entry == expected_tool)
        .count()
        != 1
    {
        return Err("capability_plane_allowlist_binding_invalid".to_string());
    }
    let request = client
        .post(url)
        .header("x-ai-platform-internal-token", internal_token)
        .header("x-ai-tenant-id", &identity.tenant_id)
        .header("x-ai-user-id", &identity.user_id)
        .header("x-ai-session-id", &identity.session_id);
    let response = trace_context
        .apply(request, &params.turn_id, &params.turn_id, None)
        .json(&build_invoke_payload(
            params,
            identity,
            capability_revision,
            snapshot_id,
            bound_dataset_ids,
            capability_allowlist,
            expected_tool,
        ))
        .send()
        .await
        .map_err(|_| "capability_plane_unavailable".to_string())?;
    if !response.status().is_success() {
        return Err("capability_plane_rejected".to_string());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "capability_plane_response_invalid".to_string())?;
    if bytes.len() > 1_048_576 {
        return Err("capability_plane_response_too_large".to_string());
    }
    let body: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "capability_plane_response_invalid".to_string())?;
    // Dynamic capabilities are intentionally read-only. A capability plane
    // response that asks for approval belongs to the typed platform approval
    // protocol, not this read-only request/response channel; rejecting it
    // keeps the tool call/result pair closed without treating it as success.
    if body
        .get("approval_required")
        .is_some_and(|value| value.as_bool() == Some(true))
        || body.get("approval_id").is_some()
    {
        return Err("capability_plane_approval_required".to_string());
    }
    if !body.get("content_items").is_some_and(Value::is_array)
        || !body.get("success").is_some_and(Value::is_boolean)
    {
        return Err("capability_plane_response_invalid".to_string());
    }
    crate::capability_execution::project_dynamic_response(
        &body["content_items"],
        body["success"].as_bool().unwrap_or(false),
    )
    .map_err(|_| "capability_plane_response_invalid".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::DynamicToolCallParams;
    use codex_protocol::ThreadId;

    fn allowlist() -> (Vec<CapabilityAllowlistEntry>, CapabilityAllowlistEntry) {
        let expected = CapabilityAllowlistEntry {
            capability_type: "knowledge".to_string(),
            name: "search_knowledge_base".to_string(),
            id: "knowledge:search".to_string(),
            version: Some("v1".to_string()),
            schema_hash: Some(format!("sha256:{}", "a".repeat(64))),
            connector_binding: None,
        };
        (vec![expected.clone()], expected)
    }

    #[test]
    fn invoke_wire_contains_snapshot_allowlist_and_expected_identity() {
        let thread_id = ThreadId::new();
        let identity = PlatformThreadIdentity::new(thread_id, "tenant-a", "user-a", "session-a");
        let params = DynamicToolCallParams {
            thread_id: thread_id.to_string(),
            turn_id: "turn-a".to_string(),
            call_id: "call-a".to_string(),
            namespace: None,
            tool: "search_knowledge_base".to_string(),
            arguments: serde_json::json!({"query": "model supplied only"}),
        };
        let (allowlist, expected_tool) = allowlist();
        let payload = build_invoke_payload(
            &params,
            &identity,
            7,
            "snapshot-a",
            &["dataset-a".to_string()],
            &allowlist,
            &expected_tool,
        );
        assert_eq!(payload["capability_allowlist"].as_array().unwrap().len(), 1);
        assert_eq!(payload["expected_tool"]["id"], "knowledge:search");
        assert_eq!(
            payload["expected_tool"]["schema_hash"],
            format!("sha256:{}", "a".repeat(64))
        );
        assert!(payload["arguments"].get("id").is_none());
    }

    #[test]
    fn rejects_userinfo_and_invalid_scheme() {
        assert!(endpoint("file:///tmp/capability").is_err());
        assert!(endpoint("http://user:pass@example/capability").is_err());
        assert!(endpoint("http://127.0.0.1:8080/internal/v2/agent-capabilities").is_ok());
    }

    #[tokio::test]
    async fn dynamic_call_projects_fake_knowledge_result_for_next_turn() {
        let app = axum::Router::new().route(
            "/invoke",
            axum::routing::post(|| async {
                axum::Json(serde_json::json!({
                    "content_items": [{"type": "input_text", "text": "Knowledge result"}],
                    "success": true
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake capability listener");
        let address = listener.local_addr().expect("fake capability address");
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let thread_id = ThreadId::new();
        let identity = PlatformThreadIdentity::new(thread_id, "tenant-a", "user-a", "session-a");
        let params = DynamicToolCallParams {
            thread_id: thread_id.to_string(),
            turn_id: "turn-a".to_string(),
            call_id: "call-a".to_string(),
            namespace: None,
            tool: "search_knowledge_base".to_string(),
            arguments: serde_json::json!({"query": "transformer"}),
        };
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("fake capability client");
        let (capability_allowlist, expected_tool) = allowlist();
        let response = invoke_dynamic_tool(
            &client,
            &params,
            &identity,
            &format!("http://{address}"),
            "runtime-token",
            7,
            "snapshot-a",
            &["dataset-a".to_string()],
            &capability_allowlist,
            &expected_tool,
            &InternalTraceContext::default(),
        )
        .await
        .expect("dynamic call should resolve");
        assert_eq!(response["success"], true);
        assert_eq!(response["contentItems"][0]["text"], "Knowledge result");
        server.abort();
    }

    #[tokio::test]
    async fn dynamic_call_rejects_approval_result_in_read_only_channel() {
        let app = axum::Router::new().route(
            "/invoke",
            axum::routing::post(|| async {
                axum::Json(serde_json::json!({
                    "content_items": [],
                    "success": true,
                    "approval_required": true,
                    "approval_id": "approval-1"
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake capability listener");
        let address = listener.local_addr().expect("fake capability address");
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let thread_id = ThreadId::new();
        let identity = PlatformThreadIdentity::new(thread_id, "tenant-a", "user-a", "session-a");
        let params = DynamicToolCallParams {
            thread_id: thread_id.to_string(),
            turn_id: "turn-a".to_string(),
            call_id: "call-a".to_string(),
            namespace: None,
            tool: "search_knowledge_base".to_string(),
            arguments: serde_json::json!({"query": "transformer"}),
        };
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("fake capability client");
        let (capability_allowlist, expected_tool) = allowlist();
        let error = invoke_dynamic_tool(
            &client,
            &params,
            &identity,
            &format!("http://{address}"),
            "runtime-token",
            7,
            "snapshot-a",
            &[],
            &capability_allowlist,
            &expected_tool,
            &InternalTraceContext::default(),
        )
        .await
        .expect_err("approval response must fail closed");
        assert_eq!(error, "capability_plane_approval_required");
        server.abort();
    }
}
