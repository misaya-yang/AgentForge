//! Source MCP discovery/schema cases. Invocation HTTP coverage is separate.
use ai_platform_gateway::{
    mcp_gateway_broker::{McpDiscovery, McpGatewayBroker, McpSecretResolver},
    mcp_http::{McpClientConfig, McpClientFactory, McpInitializedClient, McpProtocolClient},
    mcp_registry_store::{McpContext, McpFuture, McpRepository},
    mcp_registry_validation::{McpBodyKind, validate_body},
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
struct Repository {
    calls: Mutex<Vec<(String, Value)>>,
}
impl McpRepository for Repository {
    fn call<'a>(
        &'a self,
        operation: &'a str,
        context: &'a McpContext,
        args: Value,
    ) -> McpFuture<'a, Value> {
        Box::pin(async move {
            assert_eq!(context.tenant_id, "tenant-a");
            self.calls.lock().unwrap().push((operation.into(), args));
            Ok(match operation {
                "resolve_discovery_connection" => {
                    json!({"tenant_id":"tenant-a","server_id":"server-a","connection_id":"connection-a","base_url":"https://mcp.example.test","endpoint_path":"/mcp","auth_method":"oauth","secret_ref":"vault://mcp","credential_updated_at":"revision-a","response_limit_bytes":1024,"timeout_ms":3000,"protocol":"2025-11-25"})
                }
                "record_discovery" => json!({"changed":[],"removed":[],"breaking":false}),
                _ => panic!("unexpected operation"),
            })
        })
    }
}
struct Secrets {
    calls: AtomicUsize,
}
impl McpSecretResolver for Secrets {
    fn resolve<'a>(&'a self, r: &'a str) -> McpFuture<'a, String> {
        Box::pin(async move {
            assert_eq!(r, "vault://mcp");
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("ephemeral-token".into())
        })
    }
}
struct Client;
impl McpProtocolClient for Client {
    fn list_tools(&self) -> McpFuture<'_, Value> {
        Box::pin(async {
            Ok(
                json!([{"name":"upstream_search","description":"search","input_schema":{"type":"object"}}]),
            )
        })
    }
    fn close(&self) {}
}
struct Factory {
    calls: AtomicUsize,
    protocol: &'static str,
}
impl McpClientFactory for Factory {
    fn create(&self, config: McpClientConfig) -> McpFuture<'_, McpInitializedClient> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(config.base_url, "https://mcp.example.test");
        assert!(!format!("{config:?}").contains("ephemeral-token"));
        Box::pin(async move {
            Ok(McpInitializedClient {
                client: Arc::new(Client),
                protocol_version: Some(self.protocol.into()),
            })
        })
    }
}
fn context() -> McpContext {
    McpContext {
        tenant_id: "tenant-a".into(),
        user_id: "user-a".into(),
        authenticated: true,
    }
}
#[tokio::test]
async fn discovery_reuses_initialized_client_but_reauthorizes_each_request() {
    let repo = Arc::new(Repository {
        calls: Mutex::new(vec![]),
    });
    let secrets = Arc::new(Secrets {
        calls: AtomicUsize::new(0),
    });
    let factory = Arc::new(Factory {
        calls: AtomicUsize::new(0),
        protocol: "2025-11-25",
    });
    let broker = McpGatewayBroker::new(repo.clone(), secrets.clone(), factory.clone(), 300., 128);
    let first = broker
        .discover(&context(), "server-a", "connection-a", "user_delegated")
        .await
        .unwrap();
    let second = broker
        .discover(&context(), "server-a", "connection-a", "user_delegated")
        .await
        .unwrap();
    assert_eq!(first["protocol_version"], second["protocol_version"]);
    assert_eq!(factory.calls.load(Ordering::SeqCst), 1);
    assert_eq!(secrets.calls.load(Ordering::SeqCst), 1);
    {
        let calls = repo.calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .filter(|x| x.0 == "resolve_discovery_connection")
                .count(),
            2
        );
        assert_eq!(calls[1].1["tools"][0]["name"], "upstream_search");
    }
    broker.close().await;
}
#[tokio::test]
async fn protocol_mismatch_never_persists_discovery() {
    let repo = Arc::new(Repository {
        calls: Mutex::new(vec![]),
    });
    let broker = McpGatewayBroker::new(
        repo.clone(),
        Arc::new(Secrets {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(Factory {
            calls: AtomicUsize::new(0),
            protocol: "old",
        }),
        300.,
        128,
    );
    let error = broker
        .discover(&context(), "server-a", "connection-a", "user_delegated")
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "MCP_PROTOCOL_VERSION_MISMATCH");
    assert_eq!(repo.calls.lock().unwrap().len(), 1);
    broker.close().await;
}
#[test]
fn management_schemas_reject_private_destinations_and_closed_transports() {
    for url in [
        "http://mcp.example.test",
        "https://127.0.0.1/mcp",
        "https://169.254.169.254/latest/meta-data",
        "https://[::1]/mcp",
    ] {
        assert!(
            validate_body(
                McpBodyKind::ServerCreate,
                &json!({"name":"docs","base_url":url,"allowed_origins":[]})
            )
            .is_err()
        );
        assert!(validate_body(McpBodyKind::ServerUpdate, &json!({"base_url":url})).is_err());
    }
    assert!(validate_body(McpBodyKind::ServerCreate,&json!({"name":"docs","base_url":"https://mcp.example.test","transport":"stdio","allowed_origins":[]})).is_err());
}
#[test]
fn secrets_are_references_and_validation_errors_never_echo_input() {
    let valid = json!({"principal_type":"user_delegated","secret_ref":"vault://mcp","scopes":[]});
    assert!(validate_body(McpBodyKind::ConnectionCreate, &valid).is_ok());
    for body in [
        json!({"principal_type":"user_delegated","secret_ref":"https://secret.example/token"}),
        json!({"principal_type":"user_delegated","access_token":"private-canary"}),
    ] {
        let errors = validate_body(McpBodyKind::ConnectionCreate, &body).unwrap_err();
        assert!(
            !serde_json::to_string(&errors)
                .unwrap()
                .contains("private-canary")
        );
        assert!(
            !serde_json::to_string(&errors)
                .unwrap()
                .contains("secret.example")
        );
    }
}
#[test]
fn oauth_requires_complete_metadata_and_discovery_requires_explicit_connection() {
    assert!(validate_body(McpBodyKind::ServerCreate,&json!({"name":"docs","base_url":"https://mcp.example.test","allowed_origins":[],"auth_method":"oauth"})).is_err());
    assert!(
        validate_body(
            McpBodyKind::Discovery,
            &json!({"principal_type":"user_delegated"})
        )
        .is_err()
    );
}

use ai_platform_gateway::migration_compat::{
    capability::{CapabilityScope, body_hash, validate_execution_row},
    mcp_invocation::McpReadRequest,
};
fn read_request() -> McpReadRequest {
    let arguments = json!({"q":"hello"});
    McpReadRequest {
        connection_id: "connection-a".into(),
        principal_type: "user_delegated".into(),
        channel: "assistant".into(),
        runtime_name: "mcp_search".into(),
        schema_hash: format!("sha256:{}", "a".repeat(64)),
        risk_level: "low".into(),
        arguments_hash: format!("sha256:{}", body_hash(&arguments)),
        arguments,
    }
}
fn scope() -> CapabilityScope {
    CapabilityScope {
        tenant_id: "tenant-a".into(),
        user_id: "user-a".into(),
        session_id: "session-a".into(),
        execution_id: "11111111-1111-4111-8111-111111111111".into(),
        run_id: "22222222-2222-4222-8222-222222222222".into(),
        tool_call_id: "call-a".into(),
        arguments_hash: read_request().arguments_hash,
    }
}
fn execution() -> Value {
    json!({"capability_id":"mcp_search","effect":"read","approval_status":"not_required","status":"dispatched","tool_call_id":"call-a","arguments_sha256":body_hash(&json!({"q":"hello"}))})
}
#[test]
fn read_broker_accepts_active_read_execution_binding() {
    let body = read_request();
    body.validate().unwrap();
    validate_execution_row(
        &scope(),
        Some(&execution()),
        &body.runtime_name,
        "read",
        "not_required",
        &body.arguments_hash,
    )
    .unwrap();
}
#[test]
fn read_broker_rejects_write_execution_and_other_invalid_lifecycle_rows() {
    let body = read_request();
    for (field, value) in [
        ("effect", "write"),
        ("approval_status", "consumed"),
        ("status", "succeeded"),
        ("tool_call_id", "call-other"),
        ("capability_id", "other"),
        ("arguments_sha256", "wrong"),
    ] {
        let mut row = execution();
        row[field] = json!(value);
        assert_eq!(
            validate_execution_row(
                &scope(),
                Some(&row),
                &body.runtime_name,
                "read",
                "not_required",
                &body.arguments_hash
            )
            .unwrap_err()
            .status,
            403
        );
    }
    assert!(
        validate_execution_row(
            &scope(),
            None,
            &body.runtime_name,
            "read",
            "not_required",
            &body.arguments_hash
        )
        .is_err()
    );
}
#[test]
fn read_broker_rejects_tampered_arguments_before_authorization() {
    let mut body = read_request();
    body.arguments = json!({"q":"tampered"});
    assert_eq!(body.validate().unwrap_err().status, 422);
}
