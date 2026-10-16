//! Tenant CRUD and secret-redaction source contracts through the production API.
use ai_platform_auth::rbac::Rbac;
use ai_platform_gateway::{
    mcp_registry_api::*,
    mcp_registry_store::{McpContext, McpError, McpFuture, McpRepository},
    service_config_api::{ServiceConfigError, ServiceConfigFuture},
};
use axum::http::HeaderMap;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
const SERVER: &str = "11111111-1111-4111-8111-111111111111";
const CONNECTION: &str = "22222222-2222-4222-8222-222222222222";
struct Identity;
impl McpRegistryIdentityResolver for Identity {
    fn resolve<'a>(
        &'a self,
        h: &'a HeaderMap,
        _: &'a str,
    ) -> ServiceConfigFuture<'a, McpRegistryIdentity> {
        Box::pin(async move {
            Ok(McpRegistryIdentity {
                user_id: "admin-a".into(),
                tenant_id: h
                    .get("x-tenant")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("tenant-a")
                    .into(),
                authenticated: true,
                roles: vec!["admin".into()],
                permissions: vec!["console:mcp:view".into(), "console:mcp:edit".into()],
            })
        })
    }
}
struct Destination;
impl McpDestinationValidator for Destination {
    fn validate<'a>(&'a self, host: &'a str, _: u16) -> McpDestinationFuture<'a> {
        Box::pin(async move {
            if host == "private.example.test" {
                Err(McpDestinationError::Disallowed)
            } else {
                Ok(())
            }
        })
    }
}
#[derive(Default)]
struct Store {
    rows: Mutex<BTreeMap<String, Value>>,
    calls: Mutex<Vec<String>>,
    connections: Mutex<BTreeMap<String, Value>>,
}
impl McpRepository for Store {
    fn call<'a>(&'a self, op: &'a str, c: &'a McpContext, a: Value) -> McpFuture<'a, Value> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(op.into());
            let mut rows = self.rows.lock().unwrap();
            let missing = || McpError::NotFound("MCP_SERVER_NOT_FOUND".into());
            match op {
                "create_server" => {
                    let mut row = a;
                    for (k, v) in [
                        ("server_id", json!(SERVER)),
                        ("transport", json!("streamable_http")),
                        ("enabled", json!(true)),
                        ("health_status", json!("unknown")),
                        ("circuit_state", json!("closed")),
                        ("consecutive_failures", json!(0)),
                        ("created_at", json!("2026-01-01T00:00:00Z")),
                        ("updated_at", json!("2026-01-01T00:00:00Z")),
                    ] {
                        row[k] = v;
                    }
                    rows.insert(c.tenant_id.clone(), row.clone());
                    Ok(row)
                }
                "list_servers" => Ok(Value::Array(
                    rows.get(&c.tenant_id).cloned().into_iter().collect(),
                )),
                "get_server" => rows.get(&c.tenant_id).cloned().ok_or_else(missing),
                "update_server" => {
                    let row = rows.get_mut(&c.tenant_id).ok_or_else(missing)?;
                    for (k, v) in a["changes"].as_object().unwrap() {
                        row[k] = v.clone();
                    }
                    Ok(row.clone())
                }
                "delete_server" => {
                    rows.remove(&c.tenant_id).ok_or_else(missing)?;
                    Ok(Value::Null)
                }
                "create_connection" => {
                    if !rows.contains_key(&c.tenant_id) {
                        return Err(missing());
                    }
                    let mut row = a;
                    for (k, v) in [
                        ("connection_id", json!(CONNECTION)),
                        ("enabled", json!(true)),
                        ("credential_configured", json!(true)),
                        ("created_at", json!("2026-01-01T00:00:00Z")),
                        ("updated_at", json!("2026-01-01T00:00:00Z")),
                        ("access_token", json!("private-canary")),
                    ] {
                        row[k] = v;
                    }
                    self.connections
                        .lock()
                        .unwrap()
                        .insert(c.tenant_id.clone(), row.clone());
                    Ok(row)
                }
                "list_connections" => Ok(Value::Array(
                    self.connections
                        .lock()
                        .unwrap()
                        .get(&c.tenant_id)
                        .cloned()
                        .into_iter()
                        .collect(),
                )),
                _ => panic!("unexpected {op}"),
            }
        })
    }
}
fn api(s: Arc<Store>) -> McpRegistryApi {
    McpRegistryApi::new(McpRegistryDependencies {
        identity: Arc::new(Identity),
        rbac: Rbac::default(),
        enabled: true,
        repository: Some(s),
        audit: None,
        discovery: None,
        destination: Arc::new(Destination),
    })
}
fn server() -> Value {
    json!({"name":"docs","base_url":"https://mcp.example.test","allowed_origins":[]})
}
fn headers(tenant: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert("x-tenant", tenant.parse().unwrap());
    h
}
fn status(e: ServiceConfigError) -> u16 {
    match e {
        ServiceConfigError::Http { status, .. } => status,
        _ => panic!("unexpected non-HTTP error"),
    }
}
#[tokio::test]
async fn server_crud_is_tenant_scoped_and_transport_is_closed() {
    let s = Arc::new(Store::default());
    let api = api(s);
    let h = headers("tenant-a");
    let created = api
        .handle(McpRegistryOperation::CreateServer, &h, &server())
        .await
        .unwrap();
    assert_eq!(created["server"]["transport"], "streamable_http");
    assert!(created["request_id"].is_string());
    assert!(created["audit_ref"].is_string());
    let other = headers("tenant-b");
    assert_eq!(
        api.handle(McpRegistryOperation::ListServers, &other, &json!({}))
            .await
            .unwrap()["total"],
        0
    );
    assert_eq!(
        status(
            api.handle(
                McpRegistryOperation::GetServer {
                    server_id: SERVER.into()
                },
                &other,
                &json!({})
            )
            .await
            .unwrap_err()
        ),
        404
    );
    let updated = api
        .handle(
            McpRegistryOperation::UpdateServer {
                server_id: SERVER.into(),
            },
            &h,
            &json!({"name":"renamed"}),
        )
        .await
        .unwrap();
    assert_eq!(updated["server"]["name"], "renamed");
    api.handle(
        McpRegistryOperation::DeleteServer {
            server_id: SERVER.into(),
        },
        &h,
        &json!({}),
    )
    .await
    .unwrap();
    assert_eq!(
        api.handle(McpRegistryOperation::ListServers, &h, &json!({}))
            .await
            .unwrap()["total"],
        0
    );
    let mut bad = server();
    bad["transport"] = json!("stdio");
    assert_eq!(
        status(
            api.handle(McpRegistryOperation::CreateServer, &h, &bad)
                .await
                .unwrap_err()
        ),
        422
    );
}
#[tokio::test]
async fn rejected_destinations_never_reach_persistence() {
    let s = Arc::new(Store::default());
    let api = api(s.clone());
    let h = headers("tenant-a");
    for payload in [
        json!({"name":"docs","base_url":"https://127.0.0.1","allowed_origins":[]}),
        json!({"name":"docs","base_url":"https://private.example.test","allowed_origins":[]}),
        json!({"name":"docs","base_url":"https://mcp.example.test","allowed_origins":[],"auth_method":"oauth","oauth_metadata_url":"https://127.0.0.1/.well-known/oauth","oauth_resource":"https://mcp.example.test","oauth_audience":"mcp"}),
    ] {
        assert!(
            api.handle(McpRegistryOperation::CreateServer, &h, &payload)
                .await
                .is_err()
        );
    }
    assert!(
        api.handle(
            McpRegistryOperation::UpdateServer {
                server_id: SERVER.into()
            },
            &h,
            &json!({"base_url":"https://private.example.test"})
        )
        .await
        .is_err()
    );
    assert!(s.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn secret_references_are_saved_but_never_returned_in_connection_responses() {
    let s = Arc::new(Store::default());
    let api = api(s.clone());
    let h = headers("tenant-a");
    api.handle(McpRegistryOperation::CreateServer, &h, &server())
        .await
        .unwrap();
    let connection = api
        .handle(
            McpRegistryOperation::CreateConnection {
                server_id: SERVER.into(),
            },
            &h,
            &json!({"principal_type":"user_delegated","secret_ref":"vault://mcp","scopes":[]}),
        )
        .await
        .unwrap();
    assert_eq!(
        s.connections.lock().unwrap()["tenant-a"]["secret_ref"],
        "vault://mcp"
    );
    for value in [
        connection,
        api.handle(
            McpRegistryOperation::ListConnections {
                server_id: SERVER.into(),
            },
            &h,
            &json!({}),
        )
        .await
        .unwrap(),
    ] {
        let text = value.to_string();
        assert!(!text.contains("vault://mcp"));
        assert!(!text.contains("private-canary"));
        assert!(!text.contains("secret_ref"));
        assert!(!text.contains("access_token"));
    }
    let before = s.calls.lock().unwrap().len();
    for invalid in [
        json!({"principal_type":"user_delegated","secret_ref":"https://secret.example/token"}),
        json!({"principal_type":"user_delegated","access_token":"private-canary"}),
    ] {
        assert_eq!(
            status(
                api.handle(
                    McpRegistryOperation::CreateConnection {
                        server_id: SERVER.into()
                    },
                    &h,
                    &invalid
                )
                .await
                .unwrap_err()
            ),
            422
        );
    }
    assert_eq!(s.calls.lock().unwrap().len(), before);
}

use ai_platform_gateway::mcp_gateway_broker::McpDiscovery;
struct Discovery;
impl McpDiscovery for Discovery {
    fn discover<'a>(
        &'a self,
        c: &'a McpContext,
        server: &'a str,
        connection: &'a str,
        principal: &'a str,
    ) -> McpFuture<'a, Value> {
        Box::pin(async move {
            assert_eq!(c.tenant_id, "tenant-a");
            assert_eq!(server, SERVER);
            assert_eq!(connection, CONNECTION);
            assert_eq!(principal, "user_delegated");
            Ok(
                json!({"server_id":SERVER,"changed":[],"unchanged":[],"removed":[],"breaking":false}),
            )
        })
    }
}
#[tokio::test]
async fn discovery_requires_explicit_connection_and_preserves_audit_identity() {
    let s = Arc::new(Store::default());
    let api = McpRegistryApi::new(McpRegistryDependencies {
        identity: Arc::new(Identity),
        rbac: Rbac::default(),
        enabled: true,
        repository: Some(s),
        audit: None,
        discovery: Some(Arc::new(Discovery)),
        destination: Arc::new(Destination),
    });
    let mut h = headers("tenant-a");
    h.insert("x-request-id", "req-studio-123".parse().unwrap());
    let result = api
        .handle(
            McpRegistryOperation::Discover {
                server_id: SERVER.into(),
            },
            &h,
            &json!({"connection_id":CONNECTION,"principal_type":"user_delegated"}),
        )
        .await
        .unwrap();
    assert_eq!(result["server_id"], SERVER);
    assert!(result["request_id"].is_string());
    assert!(
        result["audit_ref"]
            .as_str()
            .unwrap()
            .starts_with("request:")
    );
    let error = api
        .handle(
            McpRegistryOperation::LegacyRefresh {
                server_name: "docs".into(),
                connection_id: None,
                principal_type: "user_delegated".into(),
            },
            &h,
            &json!({}),
        )
        .await
        .unwrap_err();
    assert_eq!(status(error), 422);
}
