//! Source tests/core/test_client_ip.py and core/gateway/test_circuit_breaker.py.
use ai_platform_gateway_foundation::{
    circuit_breaker::{CallError, CircuitBreaker, CircuitState},
    client_ip::{TrustedProxies, get_client_ip},
};
use axum::http::HeaderMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
#[test]
fn forwarded_addresses_require_trusted_peer_and_skip_invalid_hops() {
    let mut h = HeaderMap::new();
    h.insert("x-forwarded-for", "198.51.100.9, 10.0.0.5".parse().unwrap());
    h.insert("x-real-ip", "198.51.100.10".parse().unwrap());
    assert_eq!(
        get_client_ip(&h, Some("203.0.113.44"), None),
        "203.0.113.44"
    );
    let proxies = TrustedProxies::from_cidrs(["127.0.0.0/8", "10.0.0.0/8"]);
    assert_eq!(
        get_client_ip(&h, Some("127.0.0.1"), Some(&proxies)),
        "198.51.100.9"
    );
    h.remove("x-forwarded-for");
    assert_eq!(get_client_ip(&h, Some("127.0.0.1"), None), "198.51.100.10");
    h.remove("x-real-ip");
    h.insert("x-forwarded-for", "not-an-ip, also-bad".parse().unwrap());
    assert_eq!(get_client_ip(&h, Some("127.0.0.1"), None), "127.0.0.1");
}
#[tokio::test]
async fn circuit_breaker_state_threshold_clock_exclusion_and_call_contracts() {
    let now = Arc::new(AtomicU64::new(100));
    let clock = now.clone();
    let b = CircuitBreaker::with_clock(
        "test-svc",
        3,
        2,
        1.0,
        Arc::new(move || clock.load(Ordering::SeqCst) as f64),
    );
    assert_eq!(b.snapshot().await.state, CircuitState::Closed);
    b.on_failure(false).await;
    assert_eq!(b.snapshot().await.failure_count, 1);
    b.on_success().await;
    assert_eq!(b.snapshot().await.failure_count, 0);
    b.on_failure(true).await;
    assert_eq!(b.snapshot().await.failure_count, 0);
    assert_eq!(b.snapshot().await.state, CircuitState::Closed);
    for _ in 0..2 {
        b.on_failure(false).await;
    }
    assert_eq!(b.snapshot().await.state, CircuitState::Closed);
    b.on_failure(false).await;
    assert_eq!(b.snapshot().await.state, CircuitState::Open);
    assert!(!b.allow().await);
    let called = AtomicBool::new(false);
    let result = b
        .call(
            || {
                called.store(true, Ordering::SeqCst);
                async { Ok::<_, &str>("unexpected") }
            },
            |_| false,
        )
        .await;
    assert_eq!(
        result,
        Err(CallError::Open {
            service_id: "test-svc".into()
        })
    );
    assert!(!called.load(Ordering::SeqCst));
    now.store(102, Ordering::SeqCst);
    assert!(b.allow().await);
    assert_eq!(b.snapshot().await.state, CircuitState::HalfOpen);
    b.on_success().await;
    assert_eq!(b.snapshot().await.state, CircuitState::HalfOpen);
    b.on_success().await;
    assert_eq!(b.snapshot().await.state, CircuitState::Closed);
    assert_eq!(b.snapshot().await.failure_count, 0);
    let result = b
        .call(
            || async { Ok::<_, &str>(("arg1", "val", "result")) },
            |_| false,
        )
        .await
        .unwrap();
    assert_eq!(result, ("arg1", "val", "result"));
    let result = b.call(|| async { Err::<(), _>("boom") }, |_| false).await;
    assert_eq!(result, Err(CallError::Operation("boom")));
    assert_eq!(b.snapshot().await.failure_count, 1);
    let result = b
        .call(|| async { Err::<(), _>("excluded") }, |e| *e == "excluded")
        .await;
    assert_eq!(result, Err(CallError::Operation("excluded")));
    assert_eq!(b.snapshot().await.failure_count, 1);
}

#[test]
fn request_required_fields_content_presence_and_json_schema_contracts() {
    use ai_platform_auth::rbac::Rbac;
    use ai_platform_gateway_foundation::request_validation::{
        ContentData, ContentItem, ContentType, RequestValidator, ValidationRequest,
    };
    use serde_json::json;
    let r = Rbac::default();
    let v = RequestValidator::new(&r);
    let text = ContentItem {
        content_type: ContentType::Text,
        data: Some(ContentData::Text("hello".into())),
        url: None,
    };
    let mut request = ValidationRequest {
        service_id: "test-svc".into(),
        inputs: vec![text.clone()],
        ..Default::default()
    };
    v.validate_required_fields(&request).unwrap();
    request.service_id.clear();
    assert!(
        v.validate_required_fields(&request)
            .unwrap_err()
            .to_string()
            .contains("service_id")
    );
    request.service_id = "test-svc".into();
    request.inputs.clear();
    assert!(
        v.validate_required_fields(&request)
            .unwrap_err()
            .to_string()
            .contains("inputs")
    );
    v.validate_content_items(&[text]).unwrap();
    v.validate_content_items(&[ContentItem {
        content_type: ContentType::Text,
        data: None,
        url: Some("https://example.com".into()),
    }])
    .unwrap();
    assert!(
        v.validate_content_items(&[ContentItem {
            content_type: ContentType::Text,
            data: None,
            url: None
        }])
        .unwrap_err()
        .to_string()
        .contains("data or url")
    );
    v.validate_schema(
        &json!({"name":"test"}),
        &json!({"type":"object","properties":{"name":{"type":"string"}}}),
    )
    .unwrap();
    assert!(
        v.validate_schema(
            &json!({}),
            &json!({"type":"object","properties":{"count":{"type":"integer"}},"required":["count"]})
        )
        .is_err()
    );
    v.validate_schema(&json!({"anything":true}), &json!({}))
        .unwrap();
    v.validate_schema(&json!({"anything":true}), &json!("{\"type\":\"object\"}"))
        .unwrap();
}
