//! Source tests/api/test_architecture_status.py through the actual production router.
use ai_platform_auth::rbac::Rbac;
use ai_platform_gateway::{architecture_status_api::*, health_api::*, setup_api::*};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
struct Identity(bool);
impl SetupIdentityResolver for Identity {
    fn resolve<'a>(&'a self, _: &'a HeaderMap, _: &'a str) -> HealthApiFuture<'a, SetupIdentity> {
        Box::pin(async move {
            Ok(SetupIdentity {
                tenant_id: "tenant".into(),
                user_id: "admin".into(),
                is_authenticated: true,
                roles: vec![if self.0 { "platform_admin" } else { "user" }.into()],
                permissions: vec!["console:services:view".into()],
            })
        })
    }
}
struct Runtime(&'static str);
impl ArchitectureRuntime for Runtime {
    fn deployment_mode(&self) -> Option<String> {
        Some(self.0.into())
    }
    fn checked_at(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }
    fn image_version(&self, _: &str) -> Option<String> {
        None
    }
}
struct Dependencies;
impl HealthDependencyState for Dependencies {
    fn probe(&self) -> Option<HealthApiFuture<'_, Value>> {
        Some(Box::pin(async {
            Ok(
                json!({"core_ready":true,"core":{"database":"healthy","redis":"healthy","agent_runtime":"healthy"},"capabilities":{"knowledge_service":"degraded","capability_worker":"healthy"}}),
            )
        }))
    }
    fn cached_snapshot(&self) -> Option<Value> {
        None
    }
    fn fallback_state(&self) -> Result<HealthFallbackState, HealthApiError> {
        panic!("valid probe must not fall back")
    }
}
struct Monitor;
impl ArchitectureMonitor for Monitor {
    fn all_status(&self) -> Result<Vec<ArchitectureObservation>, HealthApiError> {
        Ok(vec![
            ArchitectureObservation {
                service_id: "frontend".into(),
                status: json!("healthy"),
                last_check: json!("http://internal.invalid"),
            },
            ArchitectureObservation {
                service_id: "qdrant".into(),
                status: json!("degraded"),
                last_check: Value::Null,
            },
        ])
    }
}
struct Cancellation;
impl HealthHttpCancellation for Cancellation {
    fn cancelled(&self) -> HealthCancellationFuture<'_> {
        Box::pin(async { StatusCode::SERVICE_UNAVAILABLE.into_response() })
    }
}
fn app(admin: bool, mode: &'static str) -> Router {
    architecture_status_router(Arc::new(ArchitectureStatusApi {
        identity: Arc::new(Identity(admin)),
        rbac: Rbac::default(),
        runtime: Arc::new(Runtime(mode)),
        dependencies: Arc::new(Dependencies),
        monitor: Arc::new(Monitor),
        cancellation: Arc::new(Cancellation),
    }))
}
async fn call(admin: bool, mode: &'static str) -> (StatusCode, Value) {
    let r = app(admin, mode)
        .oneshot(
            Request::builder()
                .uri("/admin/architecture-status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = r.status();
    let bytes = to_bytes(r.into_body(), 1_000_000).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
fn service<'a>(body: &'a Value, id: &str) -> &'a Value {
    body["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["services"].as_array().unwrap())
        .find(|s| s["service_id"] == id)
        .unwrap()
}
#[tokio::test]
async fn architecture_status_is_platform_admin_only() {
    assert_eq!(call(false, "full").await.0, StatusCode::FORBIDDEN);
}
#[tokio::test]
async fn architecture_status_is_grouped_sanitized_and_marks_jobs() {
    let (status, b) = call(true, "full").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        b["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["display_name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "Gateway Control",
            "Agent Execution",
            "Knowledge",
            "Infrastructure"
        ]
    );
    assert_eq!(service(&b, "gateway")["replicas"], 1);
    assert_eq!(service(&b, "agent-runtime")["replicas"], 1);
    assert_eq!(service(&b, "migrate")["status"], "one-shot");
    assert_eq!(service(&b, "gateway-init")["lifecycle"], "one-shot");
    assert!(
        !service(&b, "knowledge-service")["degraded_reasons"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(service(&b, "knowledge-service")["status"], "degraded");
    assert_eq!(service(&b, "gateway")["status"], "degraded");
    assert_eq!(service(&b, "agent-capability-worker")["status"], "degraded");
    assert_eq!(
        service(&b, "agent-capability-worker")["degraded_reasons"],
        json!(["optional_dependency_degraded"])
    );
    assert_eq!(service(&b, "qdrant")["status"], "degraded");
    assert_eq!(
        service(&b, "qdrant")["degraded_reasons"],
        json!(["health_contract_degraded"])
    );
    let rendered = b.to_string().to_lowercase();
    for forbidden in [
        "postgresql://",
        "redis://",
        "http://",
        "/users/",
        "traceback",
    ] {
        assert!(!rendered.contains(forbidden));
    }
}
#[tokio::test]
async fn architecture_status_falls_back_safely_for_invalid_mode() {
    let (_, b) = call(true, "invalid").await;
    assert_eq!(b["mode"], "full");
    assert_eq!(b["mode_configuration_valid"], false);
    assert_eq!(service(&b, "knowledge-worker")["active_in_mode"], true);
}
