use ai_platform_gateway_foundation::{
    protocol::{UnifiedRequest, UnifiedResponse},
    service_ownership::ServiceHandle,
    service_registry::{
        AdapterError, AdapterFuture, MemoryRegistryStorage, ProtocolAdapter, ServiceRegistry,
        service_from_value,
    },
};
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;
struct Slow {
    entered: Semaphore,
    release: Semaphore,
    calls: AtomicUsize,
}
impl ProtocolAdapter for Slow {
    fn invoke<'a>(&'a self, request: &'a UnifiedRequest) -> AdapterFuture<'a, UnifiedResponse> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
            Ok(UnifiedResponse::new(&request.request_id, "success", vec![]))
        })
    }
    fn health_check(&self) -> AdapterFuture<'_, bool> {
        Box::pin(async { Ok(true) })
    }
}
#[tokio::test]
async fn service_registry_wraps_adapter_with_configured_bulkhead() {
    let registry = ServiceRegistry::new(Arc::new(MemoryRegistryStorage::new()));
    let slow = Arc::new(Slow {
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
        calls: AtomicUsize::new(0),
    });
    let factory = slow.clone();
    registry.register_adapter(
        "slow",
        Arc::new(move |_: ServiceHandle| Ok(factory.clone() as Arc<dyn ProtocolAdapter>)),
    );
    let service=service_from_value(&json!({"service_id":"slow-service","name":"Slow Service","connector_type":"in_process","connector_config":{"module":"fixture","callable":"create"},"metadata":{"adapter_type":"slow","bulkhead":{"concurrency_limit":1,"queue_timeout_seconds":0.01}}})).unwrap();
    registry.register(service.clone()).await.unwrap();
    let adapter = registry.get_adapter(ServiceHandle::new(service)).unwrap();
    let first = adapter.clone();
    let task = tokio::spawn(async move {
        first
            .invoke(&UnifiedRequest::new("req-1", "slow-service", vec![]))
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), slow.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let second = tokio::time::timeout(
        Duration::from_secs(2),
        adapter.invoke(&UnifiedRequest::new("req-2", "slow-service", vec![])),
    )
    .await
    .unwrap();
    assert!(
        matches!(second,Err(AdapterError::BulkheadExhausted{service_id}) if service_id=="slow-service")
    );
    assert_eq!(slow.calls.load(Ordering::SeqCst), 1);
    slow.release.add_permits(1);
    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.request_id, "req-1");
    assert_eq!(result.status, "success");
    // The successful first invocation must return its permit for later work.
    slow.release.add_permits(1);
    assert_eq!(
        adapter
            .invoke(&UnifiedRequest::new("req-3", "slow-service", vec![]))
            .await
            .unwrap()
            .status,
        "success"
    );
    assert_eq!(slow.calls.load(Ordering::SeqCst), 2);
}
