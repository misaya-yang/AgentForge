//! Isolated process checks for the actual process-global tracing initializer.
use ai_platform_gateway_foundation::distributed_tracing::{
    TracingOptions, initialize, initialize_with_options,
};
use opentelemetry::trace::{Span, Tracer, TracerProvider};
#[test]
fn tracing_initializer_child_probe() {
    if std::env::var_os("NATIVE_TRACING_INIT_PROBE").is_none() {
        return;
    }
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _runtime = runtime.enter();
    let mode = std::env::var("NATIVE_TRACING_INIT_PROBE").unwrap();
    if mode == "preinstalled" {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .try_init()
            .unwrap();
    }
    let first = match mode.as_str() {
        "explicit-empty" => {
            let options = TracingOptions {
                otlp_endpoint: Some(String::new()),
                ..Default::default()
            };
            assert_eq!(options.resolved_endpoint(), "");
            initialize_with_options("svc-one", options)
        }
        "malformed" => initialize_with_options(
            "svc-one",
            TracingOptions {
                otlp_endpoint: Some("not a valid URI ://".into()),
                ..Default::default()
            },
        ),
        _ => initialize("svc-one"),
    };
    let mut span = first.tracer("fixture").start("before-shutdown");
    assert!(
        span.is_recording(),
        "unset/empty endpoint must retain real SDK tracing"
    );
    span.end();
    if mode.starts_with("log-") || mode == "preinstalled" {
        // Synthetic, allowlisted database diagnostics prove the subscriber
        // emits useful fields without including a DSN, query body, or driver error.
        tracing::warn!(
            target: "ai_platform_knowledge::ingest",
            sqlstate = "42501",
            statement_tag = "insert_segment",
            object = "segments",
            "Knowledge database operation failed"
        );
        tracing::error!(
            target: "assistant.skill_builtin",
            error_code = "builtin_internal_failure",
            "Builtin skill execution failed"
        );
    }
    first.shutdown().unwrap();
    // Shared provider shutdown is observable. A second initialization must not
    // silently replace it with a new provider under another service name.
    let second = initialize("svc-two");
    assert!(
        !second
            .tracer("fixture")
            .start("after-shutdown")
            .is_recording()
    );
}

fn run_tracing_probe(mode: &str, rust_log: Option<&str>) -> std::process::Output {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "tracing_initializer_child_probe", "--nocapture"])
        .env("NATIVE_TRACING_INIT_PROBE", mode)
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT");
    if let Some(value) = rust_log {
        command.env("RUST_LOG", value);
    } else {
        command.env_remove("RUST_LOG");
    }
    command.output().unwrap()
}

fn json_event_lines(stderr: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn event_field<'a>(event: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
    event.get(name).or_else(|| event.get("fields")?.get(name))
}

#[test]
fn tracing_initializer_emits_structured_safe_sqlstate_diagnostics_by_default() {
    let output = run_tracing_probe("log-default", None);
    assert!(
        output.status.success(),
        "child failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let events = json_event_lines(&output.stderr);
    let database = events
        .iter()
        .find(|event| {
            event_field(event, "message").and_then(serde_json::Value::as_str)
                == Some("Knowledge database operation failed")
        })
        .expect("warn-level database event should reach stderr as JSON");
    assert_eq!(
        event_field(database, "level").and_then(serde_json::Value::as_str),
        Some("WARN")
    );
    assert_eq!(
        event_field(database, "sqlstate").and_then(serde_json::Value::as_str),
        Some("42501")
    );
    assert_eq!(
        event_field(database, "statement_tag").and_then(serde_json::Value::as_str),
        Some("insert_segment")
    );
    assert_eq!(
        event_field(database, "object").and_then(serde_json::Value::as_str),
        Some("segments")
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("postgres://"));
}

#[test]
fn tracing_initializer_respects_filter_and_keeps_preinstalled_subscriber() {
    let filtered = run_tracing_probe("log-filter", Some("error"));
    assert!(filtered.status.success());
    let events = json_event_lines(&filtered.stderr);
    assert_eq!(
        events.len(),
        1,
        "RUST_LOG=error should suppress warn events"
    );
    assert_eq!(
        event_field(&events[0], "level").and_then(serde_json::Value::as_str),
        Some("ERROR")
    );
    assert_eq!(
        event_field(&events[0], "error_code").and_then(serde_json::Value::as_str),
        Some("builtin_internal_failure")
    );

    let preinstalled = run_tracing_probe("preinstalled", Some("off"));
    assert!(preinstalled.status.success());
    let output = String::from_utf8_lossy(&preinstalled.stderr);
    assert!(output.contains("Knowledge database operation failed"));
    assert!(
        !output.contains("\"fields\""),
        "the initializer must preserve the preinstalled text subscriber"
    );
}

#[test]
fn tracing_init_is_idempotent_and_unset_or_empty_endpoint_retains_sdk_provider() {
    for endpoint in [None, Some("")] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "tracing_initializer_child_probe", "--nocapture"])
            .env("NATIVE_TRACING_INIT_PROBE", "1")
            .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
            .env_remove("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
            .env_remove("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT");
        if let Some(endpoint) = endpoint {
            command.env("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "child failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn tracing_explicit_empty_overrides_env_and_malformed_exporter_is_nonfatal() {
    for mode in ["explicit-empty", "malformed"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tracing_initializer_child_probe", "--nocapture"])
            .env("NATIVE_TRACING_INIT_PROBE", mode)
            .env("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT", "")
            .env(
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                "not a valid environment URI ://",
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
#[test]
fn tracing_custom_resource_attributes_reach_actual_provider_resource_builder() {
    use opentelemetry::{Key, KeyValue, Value};
    let options = TracingOptions {
        resource_attributes: vec![KeyValue::new("deployment.environment", "test")],
        ..Default::default()
    };
    let resource = options.resource("test-service");
    assert_eq!(
        resource.get(&Key::new("service.name")),
        Some(Value::String("test-service".into()))
    );
    assert_eq!(
        resource.get(&Key::new("deployment.environment")),
        Some(Value::String("test".into()))
    );
}

#[test]
fn explicit_otlp_trace_and_metric_endpoints_are_independently_configurable() {
    let options = TracingOptions {
        otlp_endpoint: Some("http://trace-collector.invalid:4317".into()),
        otlp_metrics_endpoint: Some("http://metrics-collector.invalid:4317".into()),
        ..Default::default()
    };
    assert_eq!(
        options.resolved_endpoint(),
        "http://trace-collector.invalid:4317"
    );
    assert_eq!(
        options.resolved_metrics_endpoint(),
        "http://metrics-collector.invalid:4317"
    );
}

#[tokio::test]
async fn inbound_http_trace_and_baggage_reach_real_internal_and_provider_receivers() {
    use ai_platform_gateway_foundation::distributed_tracing::{
        client_span, current_carrier, inbound, inject_current_context,
        inject_current_trace_context, shutdown,
    };
    use axum::{
        Router,
        extract::State,
        http::{HeaderMap, HeaderValue, Request, StatusCode},
        middleware,
        response::IntoResponse,
        routing::get,
    };
    use std::time::Duration;
    use tokio::sync::mpsc;

    let _provider = initialize_with_options(
        "propagation-test",
        TracingOptions {
            otlp_endpoint: Some(String::new()),
            otlp_metrics_endpoint: Some(String::new()),
            ..Default::default()
        },
    );

    let (received_tx, mut received_rx) = mpsc::unbounded_channel::<(String, HeaderMap)>();
    let receive = move |State(sender): State<mpsc::UnboundedSender<(String, HeaderMap)>>,
                        request: Request<axum::body::Body>| async move {
        let kind = request.uri().path().trim_start_matches('/').to_owned();
        sender.send((kind, request.headers().clone())).unwrap();
        StatusCode::NO_CONTENT
    };
    let sink = Router::new()
        .route("/internal", get(receive.clone()))
        .route("/provider", get(receive))
        .with_state(received_tx);
    let sink_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sink_origin = format!("http://{}", sink_listener.local_addr().unwrap());
    let sink_task = tokio::spawn(async move {
        axum::serve(sink_listener, sink).await.unwrap();
    });

    let app = Router::new()
        .route(
            "/entry",
            get(move |State(sink_origin): State<String>| async move {
                let client = reqwest::Client::builder().build().unwrap();
                let server_traceparent = current_carrier()
                    .0
                    .get("traceparent")
                    .cloned()
                    .unwrap_or_default();
                let internal = client_span("knowledge.rpc.retrieve", async {
                    let mut headers = HeaderMap::new();
                    inject_current_context(&mut headers);
                    client
                        .get(format!("{sink_origin}/internal"))
                        .headers(headers)
                        .send()
                        .await
                })
                .await;
                let provider = client_span("gen_ai.provider.request", async {
                    let mut headers = HeaderMap::new();
                    inject_current_trace_context(&mut headers);
                    client
                        .get(format!("{sink_origin}/provider"))
                        .headers(headers)
                        .send()
                        .await
                })
                .await;
                let mut response = if internal.is_ok_and(|response| {
                    response.status().as_u16() == StatusCode::NO_CONTENT.as_u16()
                }) && provider.is_ok_and(|response| {
                    response.status().as_u16() == StatusCode::NO_CONTENT.as_u16()
                }) {
                    StatusCode::OK
                } else {
                    StatusCode::BAD_GATEWAY
                }
                .into_response();
                response.headers_mut().insert(
                    "x-test-server-traceparent",
                    HeaderValue::from_str(&server_traceparent).unwrap(),
                );
                response
            }),
        )
        .with_state(sink_origin)
        .layer(middleware::from_fn(inbound));
    let gateway_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_origin = format!("http://{}", gateway_listener.local_addr().unwrap());
    let gateway_task = tokio::spawn(async move {
        axum::serve(gateway_listener, app).await.unwrap();
    });

    let parent_trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
    let parent_span_id = "00f067aa0ba902b7";
    let response = reqwest::Client::new()
        .get(format!("{gateway_origin}/entry"))
        .header(
            "traceparent",
            format!("00-{parent_trace_id}-{parent_span_id}-01"),
        )
        .header("tracestate", "vendor=value")
        .header("baggage", "ai-platform.request-class=interactive")
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), StatusCode::OK.as_u16());
    let server_traceparent = response
        .headers()
        .get("x-test-server-traceparent")
        .unwrap()
        .to_str()
        .unwrap();
    let server_parts: Vec<_> = server_traceparent.split('-').collect();
    assert_eq!(server_parts.len(), 4);
    assert_eq!(server_parts[1], parent_trace_id);
    assert_ne!(server_parts[2], parent_span_id);

    let mut received = Vec::new();
    for _ in 0..2 {
        received.push(
            tokio::time::timeout(Duration::from_secs(2), received_rx.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    for (kind, headers) in &received {
        let traceparent = headers["traceparent"].to_str().unwrap();
        let parts: Vec<_> = traceparent.split('-').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[1], parent_trace_id);
        assert_ne!(
            parts[2], server_parts[2],
            "client span must parent downstream"
        );
        assert_eq!(headers["tracestate"], "vendor=value");
        if kind == "internal" {
            assert_eq!(headers["baggage"], "ai-platform.request-class=interactive");
        } else {
            assert_eq!(kind, "provider");
            assert!(!headers.contains_key("baggage"));
        }
    }
    gateway_task.abort();
    sink_task.abort();
    shutdown();
}
