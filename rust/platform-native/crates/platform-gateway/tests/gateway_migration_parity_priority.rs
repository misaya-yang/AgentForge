//! Exact tests/proxy/test_gateway_priority_queue.py contracts.
use ai_platform_auth::rbac::Rbac;
use ai_platform_gateway_foundation::{
    capacity::CapacityResolver,
    config_audit_writer::*,
    protocol::{ServiceCapacityConfig, ServiceConfig, ServiceDefinition},
    service_config_api::*,
    service_registry::{MemoryRegistryStorage, RegistryError, ServiceRegistry},
};
use axum::http::HeaderMap;
use serde_json::Value;
use std::sync::Arc;
struct Fixture;
impl ServiceConfigIdentityResolver for Fixture {
    fn resolve<'a>(
        &'a self,
        _: &'a HeaderMap,
        _: &'a str,
    ) -> ServiceConfigFuture<'a, ServiceConfigIdentity> {
        Box::pin(async {
            Ok(ServiceConfigIdentity {
                user_id: "admin".into(),
                tenant_id: "default".into(),
                roles: vec!["developer".into()],
                permissions: vec!["console:services:edit".into()],
            })
        })
    }
}
impl ServiceConfigExceptionText for Fixture {
    fn registry(&self, _: &RegistryError) -> String {
        "registry error".into()
    }
    fn audit(&self, _: &ConfigAuditError) -> String {
        "audit error".into()
    }
    fn internal(&self) -> String {
        "internal error".into()
    }
}
#[tokio::test]
async fn service_config_marks_priority_not_enforced() {
    let registry = Arc::new(ServiceRegistry::new(Arc::new(MemoryRegistryStorage::new())));
    registry
        .register(ServiceDefinition {
            service_id: "agent".into(),
            name: "Agent".into(),
            service_config: Some(ServiceConfig {
                capacity: ServiceCapacityConfig {
                    upstream_group: Some("langgraph_agent".into()),
                    concurrency_limit: Some(3),
                    queue_max: 0,
                    queue_timeout_ms: 100,
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .unwrap();
    let api = ServiceConfigApi::new(ServiceConfigDependencies {
        identity: Arc::new(Fixture),
        rbac: Rbac::default(),
        registry,
        database: None,
        proxy_loader: None,
        capacity_resolver: Arc::new(CapacityResolver::default()),
        admission: None,
        audit: ConfigAuditWriter::new(None, Arc::new(ProxyAwareConfigAuditClientIp::default())),
        exception_text: Arc::new(Fixture),
    });
    let response = api
        .handle(
            ServiceConfigOperation::Get {
                service_id: "agent".into(),
            },
            &HeaderMap::new(),
            &Value::Null,
            &ConfigAuditRequestContext::default(),
        )
        .await
        .unwrap();
    assert_eq!(response["config"]["priority"]["enforced"], false);
    assert_eq!(
        response["config"]["priority"]["scheduler"],
        "not_configured"
    );
    let upstream = response["capacity_status"]["budgets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == "upstream.langgraph_agent")
        .unwrap();
    assert_eq!(upstream["limit"], 3);
    assert_eq!(upstream["queue_max"], 0);
    assert_eq!(upstream["queue_timeout_ms"], 100);
}
fn dialog() -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../web/src/components/ServiceConfigDialog.tsx"),
    )
    .unwrap()
}
#[test]
fn priority_controls_are_disabled_when_backend_is_not_enforcing() {
    let source = dialog();
    for expected in [
        "priorityEnforced",
        "disabled={!priorityEnforced",
        "services.configDialog.priority.notEnforced",
    ] {
        assert!(source.contains(expected), "{expected}");
    }
}
#[test]
fn agent_capacity_and_load_balancing_are_frontend_configurable() {
    let source = dialog();
    for expected in [
        "upstreamGroupFromBudget",
        "upstream_urls_text",
        "load_balance_strategy",
        "least_connections",
    ] {
        assert!(source.contains(expected), "{expected}");
    }
}
