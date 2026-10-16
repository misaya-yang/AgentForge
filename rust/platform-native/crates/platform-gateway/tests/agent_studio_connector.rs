//! Source catalog-vs-grant connector matrix using actual repository policy and recorded SQL seams.
use ai_platform_gateway::{
    connector_principal_store::ConnectorPrincipalRepository,
    mcp_registry_store::{McpArgument, McpContext, McpDatabase, McpDatabaseFuture, McpStatement},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
struct Database {
    config: Option<Value>,
    connection: Option<Value>,
    grant: Option<Value>,
    calls: Mutex<Vec<McpStatement>>,
}
impl McpDatabase for Database {
    fn run(&self, s: McpStatement) -> McpDatabaseFuture<'_, Vec<Value>> {
        let value = match s.name {
            "authorize_connector_catalog_config" => self.config.clone(),
            "authorize_connector_catalog_user" => self.connection.clone(),
            "authorize_connector_tool" => self.grant.clone(),
            _ => panic!("unexpected statement {}", s.name),
        };
        self.calls.lock().unwrap().push(s);
        Box::pin(async move { Ok(value.into_iter().collect()) })
    }
    fn record_discovery(&self, _: McpContext, _: Value) -> McpDatabaseFuture<'_, Value> {
        panic!("discovery must not run")
    }
}
fn context() -> McpContext {
    McpContext {
        tenant_id: "tenant-a".into(),
        user_id: "user-a".into(),
        authenticated: true,
    }
}
fn config() -> Value {
    json!({"provider":"jira","mode":"live","enabled":true})
}
fn connection() -> Value {
    json!({"user_id":"user-a","provider":"jira","status":"connected"})
}
fn binding() -> Value {
    json!({"type":"connector","resource_id":"jira_search","config":{"provider":"jira","tool_name":"jira_search"}})
}
fn database(c: Option<Value>, u: Option<Value>, g: Option<Value>) -> Arc<Database> {
    Arc::new(Database {
        config: c,
        connection: u,
        grant: g,
        calls: Mutex::new(vec![]),
    })
}
#[tokio::test]
async fn enabled_connected_catalog_binding_preserves_immutable_definition_and_tenant_lookup() {
    let db = database(Some(config()), Some(connection()), None);
    let repo = ConnectorPrincipalRepository::new(true, Some(db.clone()));
    assert_eq!(
        repo.authorize_connector_binding(&context(), &binding(), "preview")
            .await
            .unwrap(),
        Some(binding())
    );
    let calls = db.calls.lock().unwrap();
    assert_eq!(
        calls.iter().map(|s| s.name).collect::<Vec<_>>(),
        vec![
            "authorize_connector_catalog_config",
            "authorize_connector_catalog_user"
        ]
    );
    assert!(calls[0].arguments == vec![McpArgument::text("jira"), McpArgument::text("tenant-a")]);
    assert!(
        calls[1].arguments
            == vec![
                McpArgument::text("tenant-a"),
                McpArgument::text("user-a"),
                McpArgument::text("jira")
            ]
    );
}
#[tokio::test]
async fn absent_disabled_ingest_disconnected_and_revoked_catalogs_are_stripped() {
    let mut disabled = config();
    disabled["enabled"] = json!(false);
    let mut ingest = config();
    ingest["mode"] = json!("ingest");
    let mut revoked = connection();
    revoked["status"] = json!("revoked");
    for (c, u) in [
        (None, None),
        (Some(disabled), Some(connection())),
        (Some(ingest), Some(connection())),
        (Some(config()), None),
        (Some(config()), Some(revoked)),
    ] {
        let repo = ConnectorPrincipalRepository::new(true, Some(database(c, u, None)));
        assert!(
            repo.authorize_connector_binding(&context(), &binding(), "preview")
                .await
                .unwrap()
                .is_none()
        );
    }
}
#[tokio::test]
async fn unauthenticated_and_public_catalog_principals_are_stripped() {
    for channel in ["hosted_public", "embed"] {
        let repo = ConnectorPrincipalRepository::new(
            true,
            Some(database(Some(config()), Some(connection()), None)),
        );
        assert!(
            repo.authorize_connector_binding(&context(), &binding(), channel)
                .await
                .unwrap()
                .is_none()
        );
    }
    let mut anonymous = context();
    anonymous.authenticated = false;
    let repo = ConnectorPrincipalRepository::new(
        true,
        Some(database(Some(config()), Some(connection()), None)),
    );
    assert!(
        repo.authorize_connector_binding(&anonymous, &binding(), "preview")
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn explicit_grant_keeps_principal_authorization_and_never_catalog_fallback() {
    let grant = json!({"grant_id":"11111111-1111-4111-8111-111111111111","provider":"confluence","principal_type":"user_delegated","owner_user_id":"user-a","scopes":["read:confluence-content.all"],"audience":"confluence","connection_metadata":{},"allowed_channels":["preview","hosted_private"],"enabled":true,"revoked_at":null});
    let b = json!({"type":"connector","resource_id":"confluence_read","config":{"provider":"confluence","tool_name":"confluence_read","principal_type":"user_delegated","grant_id":"11111111-1111-4111-8111-111111111111"}});
    let db = database(None, None, Some(grant));
    let repo = ConnectorPrincipalRepository::new(true, Some(db.clone()));
    assert_eq!(
        repo.authorize_connector_binding(&context(), &b, "preview")
            .await
            .unwrap(),
        Some(b.clone())
    );
    assert_eq!(db.calls.lock().unwrap()[0].name, "authorize_connector_tool");
    let repo = ConnectorPrincipalRepository::new(
        true,
        Some(database(Some(config()), Some(connection()), None)),
    );
    assert!(
        repo.authorize_connector_binding(&context(), &b, "preview")
            .await
            .unwrap()
            .is_none()
    );
    for removed in ["grant_id", "principal_type"] {
        let mut partial = b.clone();
        partial["config"].as_object_mut().unwrap().remove(removed);
        let db = database(Some(config()), Some(connection()), None);
        let repo = ConnectorPrincipalRepository::new(true, Some(db.clone()));
        assert!(
            repo.authorize_connector_binding(&context(), &partial, "preview")
                .await
                .unwrap()
                .is_none()
        );
        assert!(db.calls.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn repository_surfaces_source_error_codes() {
    for (c, u, channel, code) in [
        (None, None, "preview", "CONNECTOR_CATALOG_UNAVAILABLE"),
        (
            Some(config()),
            None,
            "preview",
            "CONNECTOR_CATALOG_NOT_CONNECTED",
        ),
        (
            Some(config()),
            Some(connection()),
            "embed",
            "CONNECTOR_CATALOG_PRINCIPAL_DENIED",
        ),
    ] {
        let repo = ConnectorPrincipalRepository::new(true, Some(database(c, u, None)));
        assert_eq!(
            repo.authorize_connector_catalog(&context(), "jira", "jira_search", channel)
                .await
                .unwrap_err()
                .to_string(),
            code
        );
    }
}
