//! Source tests/proxy/test_admission_metrics.py through real admission + production collector.
use ai_platform_gateway::migration_host::metrics::{AdmissionRejectionMetrics, MetricsCollector};
use ai_platform_gateway_foundation::{
    capacity::CapacityBudget,
    capacity_admission::{
        AdmissionError, AdmissionOptions, AdmissionRejectionObserver, AdmissionRequest,
        CapacityAdmissionController,
    },
};
use std::sync::Arc;
#[tokio::test]
async fn admission_rejection_updates_metrics() {
    let metrics = MetricsCollector::new();
    let controller = CapacityAdmissionController::new(
        AdmissionOptions {
            per_tenant_default_share: 1.,
            ..Default::default()
        },
        None,
    )
    .unwrap();
    assert!(controller.set_rejection_observer(Arc::new(AdmissionRejectionMetrics::new(&metrics))));
    let budget = CapacityBudget {
        key: "gateway.metrics".into(),
        limit: 1,
        queue_max: 0,
        queue_timeout_ms: 100,
        scope: "gateway".into(),
        source: "test".into(),
        enforced: true,
        source_status: "ready".into(),
        shared: false,
    };
    let first = controller
        .acquire(
            &[budget.clone()],
            &AdmissionRequest {
                tenant_id: "tenant-a".into(),
                user_id: "user-a".into(),
                service_id: "metrics-admission-service".into(),
                request_class: "sync".into(),
                request_id: "req-1".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let second = controller
        .acquire(
            &[budget.clone()],
            &AdmissionRequest {
                tenant_id: "tenant-b".into(),
                user_id: "user-b".into(),
                service_id: "metrics-admission-service".into(),
                request_class: "sync".into(),
                request_id: "req-2".into(),
                ..Default::default()
            },
        )
        .await;
    assert!(
        matches!(second,Err(AdmissionError::Rejected(ref error)) if error.code=="GATEWAY_CAPACITY_EXHAUSTED")
    );
    first.release().await;
    let counter = metrics.metric("admission_rejected_total").unwrap();
    assert_eq!(
        counter.get(&[
            ("service", "metrics-admission-service"),
            ("reason", "GATEWAY_CAPACITY_EXHAUSTED")
        ]),
        1.
    );
    let subsequent = controller
        .acquire(
            &[budget],
            &AdmissionRequest {
                service_id: "metrics-admission-service".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    subsequent.release().await;
    assert_eq!(
        counter.get(&[
            ("service", "metrics-admission-service"),
            ("reason", "GATEWAY_CAPACITY_EXHAUSTED")
        ]),
        1.
    );
    assert!(metrics.to_prometheus().contains("admission_rejected_total"));
}
#[test]
fn admission_metric_labels_are_bounded() {
    let metrics = MetricsCollector::new();
    let observer = AdmissionRejectionMetrics::new(&metrics);
    for n in 0..1100 {
        observer.rejected(&format!("service-{n}"), "unbounded-caller-reason");
    }
    observer.rejected(&"x".repeat(129), "GATEWAY_LOAD_SHED");
    let counter = metrics.metric("admission_rejected_total").unwrap();
    assert_eq!(counter.collect().len(), 1026);
    assert_eq!(
        counter.get(&[("service", "other"), ("reason", "other")]),
        76.
    );
    assert_eq!(
        counter.get(&[("service", "other"), ("reason", "GATEWAY_LOAD_SHED")]),
        1.
    );
}
