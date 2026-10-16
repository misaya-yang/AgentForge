//! Source tests/api/test_health_auth.py assertions against production health orchestration.
use ai_platform_gateway_foundation::{
    health_api::*,
    provider_health::{ProviderProbeError, ProviderProbeStatus},
    registry_health_monitor::HealthStatus,
    service_registry::{MemoryRegistryStorage, ServiceRegistry},
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct Fixture {
    snapshot: Option<Value>,
    probes: AtomicUsize,
    monitor_reads: AtomicUsize,
}
impl HealthDependencyState for Fixture {
    fn probe(&self) -> Option<HealthApiFuture<'_, Value>> {
        self.snapshot.clone().map(|value| {
            Box::pin(async move {
                self.probes.fetch_add(1, Ordering::SeqCst);
                Ok(value)
            }) as HealthApiFuture<'_, Value>
        })
    }
    fn cached_snapshot(&self) -> Option<Value> {
        None
    }
    fn fallback_state(&self) -> Result<HealthFallbackState, HealthApiError> {
        Ok(HealthFallbackState {
            runtime_present: true,
            model_plane_present: true,
            worker_running: false,
        })
    }
}
impl HealthStatusSource for Fixture {
    fn all_status(&self) -> Result<HealthRecords, HealthApiError> {
        self.monitor_reads.fetch_add(1, Ordering::SeqCst);
        if self.snapshot.as_ref().and_then(|v| v.get("degraded")) == Some(&Value::Bool(true)) {
            let mut status = HealthStatus::new("optional_connector", "unhealthy");
            status.latency = 0.1;
            status.error = Some("timeout".into());
            return Ok(vec![("optional_connector".into(), Arc::new(status))]);
        }
        Ok(vec![])
    }
    fn get_status(&self, _: &str) -> Result<Option<Arc<HealthStatus>>, HealthApiError> {
        panic!("unexpected per-service lookup")
    }
}
impl HealthProviderProbeSource for Fixture {
    fn status(&self, _: &str, _: &str, _: bool) -> Result<ProviderProbeStatus, ProviderProbeError> {
        panic!("health authorization must run before provider probes")
    }
}
fn fixture(snapshot: Option<Value>) -> (HealthApi, Arc<Fixture>) {
    let fixture = Arc::new(Fixture {
        snapshot,
        probes: AtomicUsize::new(0),
        monitor_reads: AtomicUsize::new(0),
    });
    let registry = Arc::new(ServiceRegistry::new(Arc::new(MemoryRegistryStorage::new())));
    (
        HealthApi::new(
            registry,
            fixture.clone(),
            fixture.clone(),
            None,
            fixture.clone(),
        ),
        fixture,
    )
}
fn identity(authenticated: bool, roles: &[&str]) -> HealthIdentity {
    HealthIdentity::from_resolved_identity(
        if authenticated { "user123" } else { "anon:123" }.into(),
        String::new(),
        roles.iter().map(|s| s.to_string()).collect(),
        authenticated,
    )
}
#[test]
fn basic_health_returns_only_status_without_enumeration() {
    let (api, fixture) = fixture(None);
    assert_eq!(api.gateway_health(), json!({"status":"ok"}));
    assert_eq!(fixture.monitor_reads.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.probes.load(Ordering::SeqCst), 0);
}
#[test]
fn detailed_health_requires_authenticated_admin_or_ops() {
    assert_eq!(
        require_admin(&identity(false, &[])).unwrap_err().status(),
        Some(401)
    );
    for roles in [&["user"][..], &["user", "premium"][..]] {
        assert_eq!(
            require_admin(&identity(true, roles)).unwrap_err().status(),
            Some(403)
        );
    }
    for role in ["admin", "ops"] {
        assert!(require_admin(&identity(true, &[role])).is_ok());
    }
}
#[tokio::test]
async fn initialized_runtime_is_healthy() {
    let (api, _) = fixture(None);
    let value = api
        .all_services_health(&identity(true, &["admin"]))
        .await
        .unwrap();
    assert_eq!(value["agent_runtime"]["status"], "healthy");
}
#[tokio::test]
async fn admin_sees_core_and_optional_degradation() {
    let core = json!({"auth_config":"healthy","database":"healthy","redis":"healthy","agent_runtime":"healthy","model_plane":"healthy"});
    let (api, _) = fixture(Some(
        json!({"status":"ready","core_ready":true,"degraded":true,"core":core,"capabilities":{"knowledge_service":"status_503","image_worker":"healthy"}}),
    ));
    let value = api
        .all_services_health(&identity(true, &["admin"]))
        .await
        .unwrap();
    assert_eq!(
        value["gateway_core"],
        json!({"status":"healthy","dependencies":core})
    );
    assert_eq!(
        value["knowledge_service"],
        json!({"status":"status_503","required":false})
    );
    assert_eq!(value["assistant"]["status"], "degraded");
}
#[tokio::test]
async fn non_admin_cannot_trigger_private_dependency_probe() {
    let (api, fixture) = fixture(Some(json!({})));
    let error = api
        .all_services_health(&identity(true, &["user"]))
        .await
        .unwrap_err();
    assert_eq!(error.status(), Some(403));
    assert_eq!(
        error.response_body(""),
        Some(json!({"detail":"Admin access required"}))
    );
    assert_eq!(fixture.probes.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.monitor_reads.load(Ordering::SeqCst), 0);
}
