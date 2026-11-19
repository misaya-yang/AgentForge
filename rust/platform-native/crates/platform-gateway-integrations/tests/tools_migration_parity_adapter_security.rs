use ai_platform_gateway_foundation::{
    protocol::{ContentItem, ServiceDefinition, TransportType, UnifiedRequest},
    service_ownership::ServiceHandle,
    service_registry::{MemoryRegistryStorage, ServiceRegistry},
};
use ai_platform_gateway_integrations::migration_adapters::register_adapters;
use serde_json::json;
use std::{sync::Arc, time::Duration};
#[tokio::test]
async fn remote_wait_rejects_private_callback_before_any_provider_request() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let registry = ServiceRegistry::new(Arc::new(MemoryRegistryStorage::new()));
    register_adapters(&registry);
    let mut service = ServiceDefinition::new("svc", "Fixture");
    service.connector_type = TransportType::Http;
    service.connector_config = json!({"base_url":format!("http://{}",listener.local_addr().unwrap()),"assistant_id":"asst"});
    service.metadata = json!({"adapter_type":"langgraph"});
    service.session_enabled = false;
    let adapter = registry.get_adapter(ServiceHandle::new(service)).unwrap();
    let mut request = UnifiedRequest::new("req-1", "svc", vec![ContentItem::text("fixture")]);
    request.user_id = "user".into();
    request.tenant_id = "tenant".into();
    request.callback_url = Some("http://127.0.0.1/steal".into());
    let error = tokio::time::timeout(Duration::from_secs(2), adapter.invoke(&request))
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("callback_url"), "{error}");
    assert!(matches!(listener.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
}
