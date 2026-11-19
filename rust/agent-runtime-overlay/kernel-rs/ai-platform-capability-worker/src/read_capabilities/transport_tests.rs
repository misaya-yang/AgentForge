use super::*;
use ai_platform_capability_contract::private_transport::Budget;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};
fn budget(ms: u64) -> Budget {
    let wall = crate::now_epoch_ms();
    Budget::admit(Duration::from_millis(ms), wall + ms, wall, Instant::now()).unwrap()
}
#[tokio::test]
async fn actual_post_5xx_is_never_retried_and_trace_is_sanitized() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(std::sync::Mutex::new(None));
    let count = calls.clone();
    let capture = observed.clone();
    let app = axum::Router::new().route(
        "/read",
        axum::routing::post(move |headers: axum::http::HeaderMap| {
            count.fetch_add(1, Ordering::SeqCst);
            *capture.lock().unwrap() = Some(headers);
            async { (axum::http::StatusCode::SERVICE_UNAVAILABLE, "down") }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let adapter = ReqwestReadHttpAdapter::new(
        Client::builder()
            .retry(reqwest::retry::never())
            .build()
            .unwrap(),
    );
    let parent = format!("00-{}-{}-01", "1".repeat(32), "2".repeat(16));
    let headers = vec![
        ("traceparent".into(), parent.clone()),
        ("tracestate".into(), "bad=state=extra".into()),
        ("baggage".into(), "private-data".into()),
    ];
    let result = transport::scope(
        budget(1000),
        adapter.post_internal_json(
            Url::parse(&format!("http://{address}/read")).unwrap(),
            &headers,
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(result.unwrap_err(), ReadCapabilityError::DownstreamRejected);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let headers = observed.lock().unwrap().take().unwrap();
    assert_eq!(headers["traceparent"], parent);
    assert!(headers.get("baggage").is_none());
    assert!(headers.get("tracestate").is_none());
    assert!(headers.contains_key("x-ai-budget-ms"));
    server.abort();
    let _ = server.await;
}
#[tokio::test]
async fn same_budget_covers_response_body_and_never_retries_timeout() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = [0; 1024];
        let _ = socket.read(&mut bytes).await;
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nContent-Type: application/json\r\n\r\n{").await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
    });
    let adapter = ReqwestReadHttpAdapter::new(
        Client::builder()
            .retry(reqwest::retry::never())
            .build()
            .unwrap(),
    );
    let result = transport::scope(
        budget(50),
        adapter.post_internal_json(
            Url::parse(&format!("http://{address}/read")).unwrap(),
            &[],
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(
        result.unwrap_err(),
        ReadCapabilityError::DownstreamUnavailable
    );
    server.abort();
    let _ = server.await;
}
#[tokio::test]
async fn no_verified_authority_and_legacy_rows_fail_closed() {
    let adapter = ReqwestReadHttpAdapter::new(Client::new());
    assert_eq!(
        adapter
            .post_internal_json(
                Url::parse("http://127.0.0.1:1").unwrap(),
                &[],
                serde_json::json!({})
            )
            .await
            .unwrap_err(),
        ReadCapabilityError::Scope
    );
    let result = transport::verified_record(&serde_json::json!({}), 30_000, async {
        panic!("legacy authority may not be fabricated")
    })
    .await;
    assert_eq!(result.unwrap_err(), ReadCapabilityError::Scope);
}
#[test]
fn metadata_does_not_change_business_idempotency_or_refresh_stored_expiry() {
    let old = serde_json::json!({"descriptor":{"id":"read"}});
    let mut new = old.clone();
    new[transport::EXPIRY_KEY] = serde_json::json!(9999);
    assert!(transport::bindings_match(&old, &new));
    assert!(old.get(transport::EXPIRY_KEY).is_none());
    for key in ["snapshot", "bound_dataset_ids", "business", "arbitrary"] {
        let mut changed = new.clone();
        changed[key] = serde_json::json!({"nested": "changed"});
        assert!(!transport::bindings_match(&old, &changed), "{key}");
    }
    new["descriptor"]["id"] = serde_json::json!("write");
    assert!(!transport::bindings_match(&old, &new));
}
fn old_execution(id: &str) -> crate::NewExecution {
    use ai_platform_capability_contract::{
        ApprovalPolicy, CAPABILITY_EXECUTION_SCHEMA_VERSION, CapabilityEffect,
        CapabilityExecutionStatus, CapabilityExecutionV2, canonical_json_hash,
    };
    crate::NewExecution {
        execution: CapabilityExecutionV2 {
            schema_version: CAPABILITY_EXECUTION_SCHEMA_VERSION.into(),
            execution_id: id.into(),
            lease_id: "00000000-0000-0000-0000-000000000001".into(),
            tenant_id: "t".into(),
            user_id: "u".into(),
            session_id: "s".into(),
            run_id: "00000000-0000-0000-0000-000000000002".into(),
            tool_call_id: "tool".into(),
            attempt_id: "attempt".into(),
            capability_id: "platform.read_fixture".into(),
            capability_revision: 1,
            arguments_hash: canonical_json_hash(&serde_json::json!({})).unwrap(),
            idempotency_key: "same-call".into(),
            effect: CapabilityEffect::Read,
            status: CapabilityExecutionStatus::Published,
            events_url: format!("/internal/v2/capabilities/executions/{id}/events"),
            result: None,
            error: None,
        },
        arguments: serde_json::json!({}),
        resource_binding: serde_json::json!({"snapshot":"old","business":{"nested":1}}),
        approval_policy: ApprovalPolicy::Never,
        approval_id: None,
        approval_status: "not_required".into(),
    }
}
#[tokio::test]
async fn actual_legacy_terminal_status_and_replay_survive_missing_transport_metadata() {
    use crate::{ExecutionStore, MemoryStore};
    use ai_platform_capability_contract::{CapabilityExecutionStatus, CapabilityScopeV2};
    let store = Arc::new(MemoryStore::default());
    let id = "00000000-0000-0000-0000-000000000003";
    let old = old_execution(id);
    store.reserve(old.clone()).await.unwrap();
    let scope = CapabilityScopeV2 {
        tenant_id: "t".into(),
        user_id: "u".into(),
        session_id: "s".into(),
    };
    store.dispatch(&scope, id, "fence").await.unwrap();
    store
        .append_event(
            &scope,
            id,
            "terminal",
            "terminal",
            CapabilityExecutionStatus::Succeeded,
            std::collections::BTreeMap::from([(
                "result".into(),
                serde_json::json!({"old":"receipt"}),
            )]),
            Some("fence"),
        )
        .await
        .unwrap();
    let mut replay = old;
    replay.execution.execution_id = "00000000-0000-0000-0000-000000000004".into();
    replay.resource_binding[transport::EXPIRY_KEY] = serde_json::json!(u64::MAX);
    let replayed = store.reserve(replay).await.unwrap();
    assert!(!replayed.created);
    assert!(
        replayed
            .record
            .resource_binding
            .get(transport::EXPIRY_KEY)
            .is_none()
    );
    assert_eq!(
        replayed.record.execution.status,
        CapabilityExecutionStatus::Succeeded
    );
    let state = crate::http_service::WorkerState::try_new(
        store,
        "internal-token".into(),
        vec![b'x'; 32],
        true,
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, crate::http_service::router(state))
            .await
            .unwrap()
    });
    let response = Client::new()
        .get(format!(
            "http://{address}/internal/v2/capabilities/executions/{id}"
        ))
        .header("x-ai-platform-internal-token", "internal-token")
        .header("x-ai-tenant-id", "t")
        .header("x-ai-user-id", "u")
        .header("x-ai-session-id", "s")
        .header("x-ai-budget-ms", "invalid")
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["status"], "succeeded");
    assert_eq!(value["result"], serde_json::json!({"old":"receipt"}));
    server.abort();
    let _ = server.await;
}
#[tokio::test]
async fn actual_http_resource_binding_injection_is_rejected_before_authority_changes() {
    use ai_platform_capability_contract::{
        CapabilityEffect, CreateCapabilityExecutionRequestV2, RuntimeCapabilityLeaseV1,
    };
    let descriptor = crate::fixture_catalog()
        .into_iter()
        .find(|d| d.id == "platform.read_fixture")
        .unwrap();
    let now = crate::now_epoch_ms();
    let request = CreateCapabilityExecutionRequestV2 {
        schema_version: "capability-execution-request/v2".into(),
        lease: RuntimeCapabilityLeaseV1 {
            schema_version: "runtime-capability-lease/v1".into(),
            lease_id: "00000000-0000-0000-0000-000000000001".into(),
            tenant_id: "t".into(),
            user_id: "u".into(),
            session_id: "s".into(),
            run_id: "00000000-0000-0000-0000-000000000002".into(),
            tool_call_id: "tool".into(),
            attempt_id: "attempt".into(),
            capability_id: descriptor.id.clone(),
            capability_revision: 1,
            arguments_hash: ai_platform_capability_contract::canonical_json_hash(
                &serde_json::json!({}),
            )
            .unwrap(),
            effect: CapabilityEffect::Read,
            approval_id: None,
            issued_at_epoch_ms: now,
            expires_at_epoch_ms: now + 30_000,
            nonce: "one".into(),
            signature: "invalid".into(),
        },
        idempotency_key: "one".into(),
        descriptor,
        connector_binding: None,
        arguments: serde_json::json!({}),
    };
    let mut value = serde_json::to_value(request).unwrap();
    assert!(serde_json::from_value::<CreateCapabilityExecutionRequestV2>(value.clone()).is_ok());
    value["resource_binding"] = serde_json::json!({});
    value["resource_binding"][transport::EXPIRY_KEY] = serde_json::json!(u64::MAX);
    assert!(
        serde_json::from_value::<CreateCapabilityExecutionRequestV2>(value.clone())
            .unwrap_err()
            .to_string()
            .contains("resource_binding")
    );
    let state = crate::http_service::WorkerState::try_new(
        Arc::new(crate::MemoryStore::default()),
        "internal-token".into(),
        vec![b'x'; 32],
        true,
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, crate::http_service::router(state))
            .await
            .unwrap()
    });
    let response = Client::new()
        .post(format!(
            "http://{address}/internal/v2/capabilities/executions"
        ))
        .header("x-ai-platform-internal-token", "internal-token")
        .json(&value)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 422);
    server.abort();
    let _ = server.await;
}
