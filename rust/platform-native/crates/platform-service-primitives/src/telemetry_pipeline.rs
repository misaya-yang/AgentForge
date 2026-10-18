//! Process-wide bounded exporters. Arbitrary application logs are never bridged.
use super::TracingOptions;
use opentelemetry::{
    Context, global,
    logs::{LogRecord, Logger, LoggerProvider, Severity},
    trace::TraceContextExt,
};
use opentelemetry_otlp::{Protocol, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::{
    error::OTelSdkResult,
    logs::{BatchLogProcessor, SdkLoggerProvider},
    metrics::SdkMeterProvider,
    trace::{BatchSpanProcessor, Sampler, SdkTracerProvider},
};
use std::{sync::OnceLock, time::Duration};
#[path = "telemetry_export.rs"]
mod export;
static PROVIDERS: OnceLock<TelemetryGuard> = OnceLock::new();
#[derive(Clone)]
pub struct TelemetryGuard {
    traces: SdkTracerProvider,
    metrics: Option<SdkMeterProvider>,
    logs: Option<SdkLoggerProvider>,
}
impl std::ops::Deref for TelemetryGuard {
    type Target = SdkTracerProvider;
    fn deref(&self) -> &Self::Target {
        &self.traces
    }
}
impl TelemetryGuard {
    /// All signals get a shutdown attempt even when an earlier exporter fails.
    /// Call on a blocking thread after application request/worker drain.
    pub fn shutdown(&self) -> OTelSdkResult {
        let mut result = self.traces.shutdown_with_timeout(Duration::from_secs(4));
        if let Some(metrics) = &self.metrics {
            let next = metrics.shutdown_with_timeout(Duration::from_secs(4));
            if result.is_ok() {
                result = next;
            }
        }
        if let Some(logs) = &self.logs {
            let next = logs.shutdown_with_timeout(Duration::from_secs(4));
            if result.is_ok() {
                result = next;
            }
        }
        result
    }
}
pub fn initialize_with_options(service: &str, options: TracingOptions) -> TelemetryGuard {
    PROVIDERS
        .get_or_init(|| {
            let service = service.to_owned();
            // reqwest blocking construction/destruction must not run on a Tokio worker.
            let fallback_resource = options.resource(&service);
            let providers = std::thread::Builder::new()
                .name("telemetry-init".into())
                .spawn(move || build(&service, options))
                .ok()
                .and_then(|worker| worker.join().ok())
                .and_then(Result::ok)
                .unwrap_or_else(|| {
                    eprintln!("telemetry export unavailable; retaining in-process spans");
                    TelemetryGuard {
                        traces: SdkTracerProvider::builder()
                            .with_resource(fallback_resource)
                            .build(),
                        metrics: None,
                        logs: None,
                    }
                });
            global::set_text_map_propagator(
                opentelemetry_sdk::propagation::TraceContextPropagator::new(),
            );
            global::set_tracer_provider(providers.traces.clone());
            if let Some(metrics) = &providers.metrics {
                global::set_meter_provider(metrics.clone());
            }
            providers
        })
        .clone()
}
fn build(service: &str, options: TracingOptions) -> Result<TelemetryGuard, ()> {
    let resource = options.resource(service);
    let mut traces = SdkTracerProvider::builder()
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::AlwaysOn)))
        .with_resource(resource.clone());
    let endpoint = options.resolved_endpoint();
    if endpoint.is_empty() || std::env::var("OTEL_SDK_DISABLED").as_deref() == Ok("true") {
        return Ok(TelemetryGuard {
            traces: traces.build(),
            metrics: None,
            logs: None,
        });
    }
    validate_endpoint(&endpoint)?;
    let endpoint = endpoint.trim_end_matches('/');
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| ())?;
    let trace_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_http_client(client.clone())
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(format!("{endpoint}/v1/traces"))
        .with_timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| ())?;
    let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_http_client(client.clone())
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(format!("{endpoint}/v1/metrics"))
        .with_timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| ())?;
    let log_exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_http_client(client)
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(format!("{endpoint}/v1/logs"))
        .with_timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| ())?;
    traces = traces.with_span_processor(
        BatchSpanProcessor::builder(export::Traces(trace_exporter))
            .with_batch_config(
                opentelemetry_sdk::trace::BatchConfigBuilder::default()
                    .with_max_queue_size(1024)
                    .with_max_export_batch_size(128)
                    .with_scheduled_delay(Duration::from_secs(2))
                    .build(),
            )
            .build(),
    );
    let metrics = SdkMeterProvider::builder()
        .with_resource(resource.clone())
        .with_periodic_exporter(export::Metrics(metric_exporter))
        .build();
    let logs = SdkLoggerProvider::builder()
        .with_resource(resource)
        .with_log_processor(
            BatchLogProcessor::builder(export::Logs(log_exporter))
                .with_batch_config(
                    opentelemetry_sdk::logs::BatchConfigBuilder::default()
                        .with_max_queue_size(1024)
                        .with_max_export_batch_size(128)
                        .with_scheduled_delay(Duration::from_secs(2))
                        .build(),
                )
                .build(),
        )
        .build();
    Ok(TelemetryGuard {
        traces: traces.build(),
        metrics: Some(metrics),
        logs: Some(logs),
    })
}
fn validate_endpoint(endpoint: &str) -> Result<(), ()> {
    let url = reqwest::Url::parse(endpoint).map_err(|_| ())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    Ok(())
}
pub(super) fn request_finished(context: &Context, status: u16) {
    let Some(logs) = PROVIDERS.get().and_then(|p| p.logs.as_ref()) else {
        return;
    };
    emit_request_finished(logs, context, status);
}
fn emit_request_finished(logs: &SdkLoggerProvider, context: &Context, status: u16) {
    let logger = logs.logger("agentforge.http");
    let mut record = logger.create_log_record();
    record.set_body("http.request.finished".into());
    record.set_severity_number(if status >= 500 {
        Severity::Error
    } else {
        Severity::Info
    });
    record.add_attribute("http.response.status_code", status as i64);
    let span = context.span();
    let sc = span.span_context();
    record.set_trace_context(sc.trace_id(), sc.span_id(), Some(sc.trace_flags()));
    logger.emit(record);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credential_endpoints_cannot_reach_exporter() {
        for endpoint in [
            "https://secret:token@collector/",
            "http://collector/?token=secret",
            "http://collector/#secret",
            "file:///etc/passwd",
        ] {
            assert!(validate_endpoint(endpoint).is_err());
        }
        assert!(validate_endpoint("http://collector:4318").is_ok());
    }
    #[test]
    fn disabled_pipeline_has_no_exporters_but_real_spans() {
        use opentelemetry::trace::{Span, Tracer, TracerProvider};
        let guard = build(
            "test",
            TracingOptions {
                otlp_endpoint: Some(String::new()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(guard.metrics.is_none());
        assert!(guard.logs.is_none());
        let mut span = guard.traces.tracer("test").start("test");
        assert!(span.is_recording());
        span.end();
        guard.shutdown().unwrap();
    }
    #[test]
    fn real_sdk_exporters_flush_three_signals_to_loopback_without_private_values() {
        use opentelemetry::{
            metrics::MeterProvider,
            trace::{Span, Tracer, TracerProvider},
        };
        use std::{
            io::{Read, Write},
            net::TcpListener,
            sync::{Arc, Mutex},
            time::Instant,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let capture = received.clone();
        let receiver = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < deadline && capture.lock().unwrap().len() < 3 {
                let Ok((mut socket, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 4096];
                let header_end = loop {
                    let n = socket.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                    assert!(bytes.len() < 4 * 1024 * 1024);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let header = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
                let length: usize = header
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                assert!(length > 0 && length < 4 * 1024 * 1024);
                while bytes.len() < header_end + length {
                    let n = socket.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                }
                let path = header
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_owned();
                capture
                    .lock()
                    .unwrap()
                    .push((path, bytes[header_end..header_end + length].to_vec()));
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-protobuf\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            }
        });
        let guard = build(
            "loopback-test",
            TracingOptions {
                otlp_endpoint: Some(format!("http://{address}")),
                ..Default::default()
            },
        )
        .unwrap();
        let mut span = guard.traces.tracer("test").start("chat");
        span.set_attribute(opentelemetry::KeyValue::new(
            "gen_ai.operation.name",
            "chat",
        ));
        let context = Context::new().with_span(span);
        guard
            .metrics
            .as_ref()
            .unwrap()
            .meter("test")
            .u64_counter("test.tokens")
            .build()
            .add(7, &[]);
        emit_request_finished(guard.logs.as_ref().unwrap(), &context, 200);
        context.span().end();
        guard.shutdown().unwrap();
        receiver.join().unwrap();
        let exports = received.lock().unwrap();
        assert_eq!(exports.len(), 3);
        for path in ["/v1/traces", "/v1/metrics", "/v1/logs"] {
            let body = &exports.iter().find(|(p, _)| p == path).unwrap().1;
            let text = String::from_utf8_lossy(body);
            assert!(text.contains("loopback-test"));
            assert!(text.contains("1.37.0"));
            for seed in ["private-prompt", "private-credential", "authorization"] {
                assert!(!text.contains(seed));
            }
        }
    }
}
