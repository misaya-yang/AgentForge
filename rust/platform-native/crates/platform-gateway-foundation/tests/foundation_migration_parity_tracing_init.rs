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

#[test]
fn tracing_init_is_idempotent_and_unset_or_empty_endpoint_retains_sdk_provider() {
    for endpoint in [None, Some("")] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "tracing_initializer_child_probe", "--nocapture"])
            .env("NATIVE_TRACING_INIT_PROBE", "1")
            .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT");
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
