//! In-memory worker bodies; these tests do not open HTTP or database connections.
use super::*;
use ai_platform_gateway_foundation::sdk_capabilities::SdkCapabilityLease;
use futures_util::FutureExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::oneshot;

const RUN: &str = "00000000-0000-4000-8000-000000000001";

async fn fixture(
    body: reqwest::Body,
    framework: &'static str,
    knowledge: bool,
    tools: Value,
) -> (NativeSdkStream, oneshot::Receiver<&'static str>) {
    let response = axum::http::Response::builder()
        .status(200)
        .body(body)
        .unwrap();
    let lines = Lines::new(HttpResponse {
        response: response.into(),
        permit: None,
    })
    .unwrap();
    let (finished, receiver) = oneshot::channel();
    let capability = Some(SdkCapabilityLease {
        bridge: json!({"run_id":RUN,"tools":tools,"grant":"private-test-grant"}),
        finished: Some(finished),
    });
    (
        NativeSdkStream {
            audit: sdk_usage_audit::UsageAudit::for_stream_test().await,
            model: "model".into(),
            capability,
            lines,
            request_id: "request".into(),
            index: 0,
            done: false,
            size: 0,
            data: String::new(),
            text: String::new(),
            framework,
            knowledge,
        },
        receiver,
    )
}

fn events(values: &[Value]) -> reqwest::Body {
    values
        .iter()
        .map(|value| format!("data: {value}\n\n"))
        .collect::<String>()
        .into()
}

fn assert_identity(part: &StreamChunk, framework: &str, profile: &str) {
    let metadata = part.metadata.as_ref().unwrap();
    assert_eq!(metadata["assistant_run_id"], RUN);
    assert_eq!(metadata["sdk_framework"], framework);
    assert_eq!(metadata["sdk_tools_profile"], profile);
    assert!(!metadata.contains_key("grant"));
    assert!(
        !serde_json::to_string(metadata)
            .unwrap()
            .contains("private-test-grant")
    );
}

#[tokio::test]
async fn identity_precedes_first_body_poll_for_every_framework_and_profile() {
    for framework in ["openai_agents", "claude_agent_sdk"] {
        for (knowledge, tools, profile) in [
            (false, json!([]), "pure_readonly_v1"),
            (true, json!([]), "scoped_knowledge_read_v1"),
            (false, json!([{"id":"tool"}]), "governed_capabilities_v1"),
        ] {
            let polls = Arc::new(AtomicUsize::new(0));
            let observed = polls.clone();
            let body = reqwest::Body::wrap_stream(futures_util::stream::poll_fn(move |_| {
                observed.fetch_add(1, Ordering::SeqCst);
                std::task::Poll::<Option<Result<Vec<u8>, std::io::Error>>>::Pending
            }));
            let (mut stream, mut finished) = fixture(body, framework, knowledge, tools).await;
            let part = stream
                .next()
                .now_or_never()
                .expect("identity must not wait")
                .unwrap()
                .unwrap();
            assert_identity(&part, framework, profile);
            assert_eq!(part.chunk_index, 0);
            assert_eq!(part.event_type, StreamEventType::TextDelta);
            assert_eq!(data_value(&part.content), "");
            assert!(!part.is_final);
            assert!(part.tool_call.is_none());
            assert!(!part.metadata.as_ref().unwrap().contains_key("usage"));
            assert!(
                !part
                    .metadata
                    .as_ref()
                    .unwrap()
                    .contains_key("sdk_lifecycle")
            );
            assert_eq!(polls.load(Ordering::SeqCst), 0);
            assert!(matches!(
                finished.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
            assert_eq!(stream.index, 1);
            assert!(stream.text.is_empty());
            assert_eq!(stream.size, 0);
            assert!(
                stream.next().now_or_never().is_none(),
                "identity must be emitted once"
            );
            assert!(polls.load(Ordering::SeqCst) > 0);
            Box::new(stream).close().await.unwrap();
            assert_eq!(finished.await.unwrap(), "cancelled");
        }
    }
}

#[tokio::test]
async fn identity_preserves_content_order_and_single_terminal_usage() {
    let body = events(&[
        json!({"event":"delta","text":"hello","run_id":"worker-forgery"}),
        json!({"event":"completed","streamed_text":"hello world","run_id":"worker-forgery",
            "usage":{"input_tokens":7,"output_tokens":2,"total_tokens":9,
                "sdk_usage":{"state":"reported","assistant_run_id":"worker-forgery"}}}),
        json!({"event":"completed","text":"must never be emitted","usage":{"total_tokens":999}}),
    ]);
    let (mut stream, finished) = fixture(body, "openai_agents", false, json!([])).await;
    let mut parts = Vec::new();
    while let Some(part) = stream.next().await.unwrap() {
        parts.push(part);
    }
    assert_eq!(parts.len(), 3);
    assert_eq!(
        parts
            .iter()
            .map(|part| part.chunk_index)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        parts
            .iter()
            .map(|part| data_value(&part.content))
            .collect::<Vec<_>>(),
        vec![json!(""), json!("hello"), json!(" world")]
    );
    for part in &parts {
        assert_identity(part, "openai_agents", "pure_readonly_v1");
    }
    assert_eq!(parts.iter().filter(|part| part.is_final).count(), 1);
    assert_eq!(
        parts
            .iter()
            .filter(|part| part.metadata.as_ref().unwrap().contains_key("usage"))
            .count(),
        1
    );
    assert_eq!(parts[2].event_type, StreamEventType::Final);
    let usage = &parts[2].metadata.as_ref().unwrap()["usage"];
    assert_eq!(usage["input_tokens"], 7);
    assert_eq!(usage["total_tokens"], 9);
    assert_eq!(usage["sdk_usage"]["assistant_run_id"], RUN);
    assert_eq!(
        usage["sdk_usage"]["audit_persistence"],
        "unresolved_admission"
    );
    assert!(stream.next().await.unwrap().is_none());
    assert_eq!(finished.await.unwrap(), "completed");
    Box::new(stream).close().await.unwrap();
}

#[tokio::test]
async fn every_governed_lifecycle_chunk_has_the_same_trusted_top_level_identity() {
    let body = events(&[
        json!({"event":"approval_required","run_id":RUN,"tool_call_id":"call"}),
        json!({"event":"approval_result","run_id":RUN,"tool_call_id":"call","status":"approved"}),
        json!({"event":"tool_result","run_id":RUN,"tool_call_id":"call","status":"completed"}),
    ]);
    let (mut stream, finished) =
        fixture(body, "claude_agent_sdk", true, json!([{"id":"tool"}])).await;
    assert_identity(
        &stream.next().await.unwrap().unwrap(),
        "claude_agent_sdk",
        "governed_capabilities_v1",
    );
    for (index, event, kind) in [
        (1, "approval_required", StreamEventType::ApprovalRequired),
        (2, "approval_result", StreamEventType::ApprovalResult),
        (3, "tool_result", StreamEventType::ToolResult),
    ] {
        let part = stream.next().await.unwrap().unwrap();
        assert_identity(&part, "claude_agent_sdk", "governed_capabilities_v1");
        assert_eq!(part.chunk_index, index);
        assert_eq!(part.event_type, kind);
        assert!(!part.is_final);
        assert_eq!(data_value(&part.content), "");
        let metadata = part.metadata.as_ref().unwrap();
        assert_eq!(metadata["sdk_lifecycle"]["event"], event);
        assert_eq!(metadata["sdk_lifecycle"]["run_id"], RUN);
        assert!(!metadata.contains_key("usage"));
    }
    Box::new(stream).close().await.unwrap();
    assert_eq!(finished.await.unwrap(), "cancelled");
}

#[tokio::test]
async fn lifecycle_still_rejects_unbound_run_and_zero_tool_authority() {
    for (run, tools) in [("worker-forgery", json!([{"id":"tool"}])), (RUN, json!([]))] {
        let body =
            events(&[json!({"event":"approval_required","run_id":run,"tool_call_id":"call"})]);
        let (mut stream, finished) = fixture(body, "openai_agents", false, tools).await;
        assert_eq!(
            stream.next().await.unwrap().unwrap().metadata.unwrap()["assistant_run_id"],
            RUN
        );
        assert!(stream.next().await.is_err());
        assert!(stream.next().await.unwrap().is_none());
        assert_eq!(finished.await.unwrap(), "failed");
    }
}

#[tokio::test]
async fn missing_lease_never_fabricates_an_identity_chunk() {
    let body = events(&[json!({"event":"delta","text":"content","run_id":"worker-forgery"})]);
    let (mut stream, finished) = fixture(body, "openai_agents", false, json!([])).await;
    drop(stream.capability.take());
    assert_eq!(finished.await.unwrap(), "cancelled");
    let part = stream.next().await.unwrap().unwrap();
    assert_eq!(part.chunk_index, 0);
    assert_eq!(data_value(&part.content), "content");
    assert!(!part.metadata.unwrap().contains_key("assistant_run_id"));
    Box::new(stream).close().await.unwrap();
}

#[tokio::test]
async fn completion_without_deltas_still_follows_identity_exactly_once() {
    for text in ["", "answer"] {
        let body = events(&[json!({"event":"completed","text":text})]);
        let (mut stream, finished) = fixture(body, "claude_agent_sdk", true, json!([])).await;
        let identity = stream.next().await.unwrap().unwrap();
        let terminal = stream.next().await.unwrap().unwrap();
        assert_eq!(identity.chunk_index, 0);
        assert!(!identity.is_final);
        assert_eq!(data_value(&identity.content), "");
        assert_eq!(terminal.chunk_index, 1);
        assert!(terminal.is_final);
        assert_eq!(terminal.event_type, StreamEventType::Final);
        assert_eq!(data_value(&terminal.content), text);
        assert_identity(&terminal, "claude_agent_sdk", "scoped_knowledge_read_v1");
        let usage = &terminal.metadata.as_ref().unwrap()["usage"];
        assert_eq!(usage["sdk_usage"]["state"], "unavailable");
        assert!(usage.get("input_tokens").is_none());
        assert!(stream.next().await.unwrap().is_none());
        assert_eq!(finished.await.unwrap(), "completed");
        Box::new(stream).close().await.unwrap();
    }
}

async fn assert_sdk_stream_history(values: &[Value], expected: &str, expected_parts: &[&str]) {
    use ai_platform_gateway_foundation::{
        dispatcher::StreamStatus, stream_statistics::StreamStatistics,
    };
    for framework in ["openai_agents", "claude_agent_sdk"] {
        let (mut stream, finished) = fixture(events(values), framework, false, json!([])).await;
        let mut stats = StreamStatistics::sdk();
        let mut parts = Vec::new();
        while let Some(part) = stream.next().await.unwrap() {
            parts.push(data_value(&part.content));
            let forwarded = stats.observe(part, (parts.len() * 10) as i64);
            assert!(!forwarded.is_final);
        }
        assert_eq!(
            parts,
            expected_parts
                .iter()
                .map(|part| json!(part))
                .collect::<Vec<_>>()
        );
        stats.stream_end("request", "input", 100).unwrap();
        let result = stats
            .finalize(StreamStatus::Success, "input", 101, Some(102))
            .unwrap();
        assert_eq!(result.output_text, expected);
        let metadata = result.session_metadata.unwrap();
        assert_eq!(metadata["assistant_run_id"], RUN);
        assert_eq!(metadata["sdk_framework"], framework);
        assert_eq!(metadata["sdk_stream_status"], "completed");
        assert_eq!(metadata["stats"]["sdk_usage"]["state"], "unavailable");
        assert!(metadata["stats"].get("total_tokens").is_none());
        assert!(stream.next().await.unwrap().is_none());
        assert_eq!(finished.await.unwrap(), "completed");
        Box::new(stream).close().await.unwrap();
    }
}

#[tokio::test]
async fn final_only_sdk_text_reaches_gateway_history() {
    assert_sdk_stream_history(
        &[json!({"event":"completed","text":"answer"})],
        "answer",
        &["", "answer"],
    )
    .await;
}

#[tokio::test]
async fn sdk_delta_and_final_suffix_reach_gateway_history_exactly_once() {
    assert_sdk_stream_history(
        &[
            json!({"event":"delta","text":"hello"}),
            json!({"event":"completed","streamed_text":"hello world"}),
            json!({"event":"completed","text":"must never be emitted"}),
        ],
        "hello world",
        &["", "hello", " world"],
    )
    .await;
}

#[tokio::test]
async fn unfinished_oversized_sdk_line_fails_the_lease_without_a_success_or_eof() {
    for framework in ["openai_agents", "claude_agent_sdk"] {
        let polls = Arc::new(AtomicUsize::new(0));
        let observed = polls.clone();
        let mut remaining = 65;
        let body = reqwest::Body::wrap_stream(futures_util::stream::poll_fn(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            if remaining == 0 {
                return std::task::Poll::Pending;
            }
            remaining -= 1;
            std::task::Poll::Ready(Some(Ok::<_, std::io::Error>(vec![b'x'; 65536])))
        }));
        let (mut stream, finished) = fixture(body, framework, false, json!([])).await;
        let identity = stream.next().await.unwrap().unwrap();
        assert!(!identity.is_final);
        assert_eq!(polls.load(Ordering::SeqCst), 0);
        let error = stream.next().now_or_never().expect("must reject an unfinished line").unwrap_err();
        assert!(matches!(error, DispatchError::AdapterValidationFailed("provider_stream_line_too_large")));
        assert_eq!(polls.load(Ordering::SeqCst), 65);
        assert_eq!(finished.await.unwrap(), "failed");
        assert!(stream.done);
        assert!(stream.next().await.unwrap().is_none());
        assert_eq!(polls.load(Ordering::SeqCst), 65);
        Box::new(stream).close().await.unwrap();
    }
}

const WORKER_FACTORY_TEST_KEY: &str = "sdk-worker-factory-test-encryption-key";
const WORKER_FACTORY_TEST_TOKEN: &str = "config-stdin-worker-token-0123456789";

fn configured_sdk_service() -> ServiceHandle {
    let mut service = crate::protocol::ServiceDefinition::new(
        "sdk-worker-config-test",
        "SDK worker config test",
    );
    service.session_enabled = true;
    service.max_retries = 0;
    service.metadata = json!({
        "adapter_type": "openai_agents",
        "sdk_agent_worker_url": "http://127.0.0.1:1",
        "sdk_agent_worker_token": "service-metadata-must-not-select-worker",
        "sdk_agent": {
            "model": "fixture-model",
            "instructions": "",
            "max_turns": 4
        }
    });
    service.connector_config = json!({"sdk_provider_api_key": "synthetic-provider-key-012345"});
    ai_platform_gateway_foundation::sdk_agent_config::validate_and_seal(
        &mut service,
        WORKER_FACTORY_TEST_KEY,
    )
    .unwrap();
    ServiceHandle::new(service)
}

fn sdk_worker_test_registry(
    environment: &std::collections::BTreeMap<String, String>,
) -> crate::service_registry::ServiceRegistry {
    let registry = crate::service_registry::ServiceRegistry::new(Arc::new(
        crate::service_registry::MemoryRegistryStorage::new(),
    ));
    registry
        .configure_sdk_agent_key(WORKER_FACTORY_TEST_KEY.into())
        .unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://sdk:sdk@127.0.0.1/sdk")
        .unwrap();
    let worker_config = sdk_session_cleanup::SdkAgentWorkerConfig::from_operator_environment(
        environment,
    );
    crate::migration_adapters::register_sdk_session_adapters(&registry, &pool, worker_config);
    registry
}

async fn capture_sdk_worker_health(
    axum::extract::State(observed_authorization): axum::extract::State<
        Arc<std::sync::Mutex<Option<String>>>,
    >,
    headers: axum::http::HeaderMap,
) -> axum::Json<Value> {
    *observed_authorization.lock().unwrap() = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    axum::Json(json!({"ready": true, "protocol": "ai-platform/sdk-agent/v1"}))
}

#[tokio::test]
async fn sdk_factory_uses_explicit_config_document_worker_origin_and_token() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let observed_authorization = Arc::new(std::sync::Mutex::new(None));
    let app = axum::Router::new()
        .route(
            "/health/ready",
            axum::routing::get(capture_sdk_worker_health),
        )
        .with_state(observed_authorization.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("test SDK worker server");
    });

    // This map is the explicit GatewayDocument.environment passed to bootstrap.
    // The adapter must not consult absent or different process environment values.
    let document_environment = std::collections::BTreeMap::from([
        ("SDK_AGENT_WORKER_URL".into(), origin),
        (
            "SDK_AGENT_WORKER_TOKEN".into(),
            WORKER_FACTORY_TEST_TOKEN.into(),
        ),
    ]);
    let registry = sdk_worker_test_registry(&document_environment);
    let adapter = registry.get_adapter(configured_sdk_service()).unwrap();

    assert!(adapter.health_check().await.unwrap());
    let expected_authorization = format!("Bearer {WORKER_FACTORY_TEST_TOKEN}");
    assert_eq!(
        observed_authorization.lock().unwrap().as_deref(),
        Some(expected_authorization.as_str())
    );
    server.abort();
}

#[tokio::test]
async fn sdk_factory_fails_closed_for_missing_or_invalid_operator_worker_config() {
    let valid_token = WORKER_FACTORY_TEST_TOKEN.to_owned();
    for environment in [
        std::collections::BTreeMap::new(),
        std::collections::BTreeMap::from([
            ("SDK_AGENT_WORKER_URL".into(), "http://worker.internal".into()),
            ("SDK_AGENT_WORKER_TOKEN".into(), valid_token.clone()),
        ]),
        std::collections::BTreeMap::from([
            ("SDK_AGENT_WORKER_URL".into(), "http://127.0.0.1:8096/path".into()),
            ("SDK_AGENT_WORKER_TOKEN".into(), valid_token.clone()),
        ]),
        std::collections::BTreeMap::from([
            ("SDK_AGENT_WORKER_URL".into(), "https://worker.example".into()),
            ("SDK_AGENT_WORKER_TOKEN".into(), "short-token".into()),
        ]),
    ] {
        let registry = sdk_worker_test_registry(&environment);
        assert!(registry.get_adapter(configured_sdk_service()).is_err());
    }
}
