use ai_platform_gateway_foundation::{
    protocol::{UnifiedRequest, UnifiedResponse},
    service_ownership::ServiceHandle,
    service_registry::{
        AdapterError, AdapterFuture, MemoryRegistryStorage, ProtocolAdapter, RegistryError,
        ServiceRegistry, service_from_value,
    },
};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct Adapter {
    attempts: AtomicUsize,
}
impl ProtocolAdapter for Adapter {
    fn invoke<'a>(&'a self, _: &'a UnifiedRequest) -> AdapterFuture<'a, UnifiedResponse> {
        Box::pin(async { Err(AdapterError::UnsupportedOperation) })
    }
    fn health_check(&self) -> AdapterFuture<'_, bool> {
        Box::pin(async { Ok(true) })
    }
    fn shutdown(&self) -> AdapterFuture<'_, ()> {
        Box::pin(async move {
            if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(AdapterError::UnsupportedOperation)
            } else {
                Ok(())
            }
        })
    }
}
#[tokio::test]
async fn actual_registry_and_bulkhead_forward_shutdown_and_allow_failure_retry() {
    let registry = ServiceRegistry::new(Arc::new(MemoryRegistryStorage::new()));
    let adapter = Arc::new(Adapter {
        attempts: AtomicUsize::new(0),
    });
    let factory_adapter = adapter.clone();
    registry.register_adapter(
        "shutdown-test",
        Arc::new(move |_: ServiceHandle| Ok(factory_adapter.clone() as Arc<dyn ProtocolAdapter>)),
    );
    let service = ServiceHandle::new(service_from_value(&json!({"service_id":"shutdown-test", "metadata":{"adapter_type":"shutdown-test", "bulkhead":{"concurrency_limit":1}}})).unwrap());
    let _handle = registry.get_adapter(service.clone()).unwrap();
    assert!(registry.shutdown().await.is_err());
    assert_eq!(adapter.attempts.load(Ordering::SeqCst), 1);
    assert!(matches!(
        registry.get_adapter(service),
        Err(RegistryError::InvalidConfig(
            "service_registry_shutting_down"
        ))
    ));
    registry.shutdown().await.unwrap();
    registry.shutdown().await.unwrap();
    assert_eq!(adapter.attempts.load(Ordering::SeqCst), 2);
}
