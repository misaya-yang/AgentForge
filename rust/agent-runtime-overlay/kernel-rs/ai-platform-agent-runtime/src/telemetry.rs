//! Content-free OTLP export. Never bridge arbitrary tracing fields into OTLP.
use opentelemetry::{
    Context, KeyValue, global,
    logs::{LogRecord, Logger, LoggerProvider, Severity},
    trace::{
        Span, SpanContext, SpanId, Status, TraceContextExt, TraceFlags, TraceId, TraceState, Tracer,
    },
};
use opentelemetry_otlp::{Protocol, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::{
    Resource,
    logs::{BatchLogProcessor, SdkLoggerProvider},
    metrics::SdkMeterProvider,
    trace::{BatchConfigBuilder, BatchSpanProcessor, SdkTracerProvider},
};
#[path = "telemetry_export.rs"]
mod telemetry_export;
use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

/// GenAI experimental conventions are deliberately pinned rather than following latest.
pub const SEMCONV_VERSION: &str = "1.37.0";
const SCOPE: &str = "agentforge.gen_ai";
const SCHEMA_URL: &str = "https://opentelemetry.io/schemas/1.37.0";
static LOGS: OnceLock<SdkLoggerProvider> = OnceLock::new();

pub struct TelemetryGuard {
    traces: SdkTracerProvider,
    metrics: SdkMeterProvider,
    logs: SdkLoggerProvider,
}
impl TelemetryGuard {
    /// SDK batch workers flush on dedicated threads; never block the executor.
    pub async fn shutdown(self) {
        let _ = tokio::task::spawn_blocking(move || drop(self)).await;
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        for result in [
            self.traces.shutdown_with_timeout(Duration::from_secs(4)),
            self.metrics.shutdown_with_timeout(Duration::from_secs(4)),
            self.logs.shutdown_with_timeout(Duration::from_secs(4)),
        ] {
            if result.is_err() {
                tracing::warn!(code = "OTEL_SHUTDOWN_FAILED", "telemetry flush failed");
            }
        }
    }
}

/// No endpoint is resolved and no exporter exists until explicitly enabled.
pub async fn init(service: &'static str) -> Result<Option<TelemetryGuard>, String> {
    if std::env::var("AI_PLATFORM_OTEL_ENABLED").as_deref() != Ok("true")
        || std::env::var("OTEL_SDK_DISABLED").as_deref() == Ok("true")
    {
        return Ok(None);
    }
    match tokio::task::spawn_blocking(move || init_enabled(service)).await {
        Ok(Ok(guard)) => Ok(guard),
        _ => {
            // Telemetry configuration/transport must never reject business startup.
            tracing::warn!(code = "OTEL_INIT_FAILED", "telemetry export unavailable");
            Ok(None)
        }
    }
}
fn init_enabled(service: &'static str) -> Result<Option<TelemetryGuard>, String> {
    let endpoint =
        std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").map_err(|_| "otel_endpoint_missing")?;
    validate_endpoint(&endpoint)?;
    let endpoint = endpoint.trim_end_matches('/');
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "otel_http_client_failed")?;
    let resource = Resource::builder_empty()
        .with_attributes([
            KeyValue::new("service.name", service),
            KeyValue::new("gen_ai.semconv.version", SEMCONV_VERSION),
        ])
        .build();
    let trace_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_http_client(client.clone())
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(format!("{endpoint}/v1/traces"))
        .with_timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| "otel_exporter_init_failed")?;
    let processor = BatchSpanProcessor::builder(telemetry_export::Traces(trace_exporter))
        .with_batch_config(
            BatchConfigBuilder::default()
                .with_max_queue_size(1024)
                .with_max_export_batch_size(128)
                .with_scheduled_delay(Duration::from_secs(2))
                .build(),
        )
        .build();
    let traces = SdkTracerProvider::builder()
        .with_resource(resource.clone())
        .with_span_processor(processor)
        .build();
    let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_http_client(client.clone())
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(format!("{endpoint}/v1/metrics"))
        .with_timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| "otel_exporter_init_failed")?;
    let metrics = SdkMeterProvider::builder()
        .with_resource(resource.clone())
        .with_periodic_exporter(telemetry_export::Metrics(metric_exporter))
        .build();
    let log_exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_http_client(client.clone())
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(format!("{endpoint}/v1/logs"))
        .with_timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| "otel_exporter_init_failed")?;
    let processor = BatchLogProcessor::builder(telemetry_export::Logs(log_exporter))
        .with_batch_config(
            opentelemetry_sdk::logs::BatchConfigBuilder::default()
                .with_max_queue_size(1024)
                .with_max_export_batch_size(128)
                .with_scheduled_delay(Duration::from_secs(2))
                .build(),
        )
        .build();
    let logs = SdkLoggerProvider::builder()
        .with_resource(resource)
        .with_log_processor(processor)
        .build();
    LOGS.set(logs.clone())
        .map_err(|_| "otel_already_initialized")?;
    global::set_tracer_provider(traces.clone());
    global::set_meter_provider(metrics.clone());
    Ok(Some(TelemetryGuard {
        traces,
        metrics,
        logs,
    }))
}

#[derive(Clone, Copy)]
pub enum OperationKind {
    InvokeAgent,
    Chat,
    ExecuteTool,
    ApprovalWait,
}
impl OperationKind {
    fn name(self) -> &'static str {
        match self {
            Self::InvokeAgent => "invoke_agent",
            Self::Chat => "chat",
            Self::ExecuteTool => "execute_tool",
            Self::ApprovalWait => "approval_wait",
        }
    }
}
/// Fixed attributes only: callers cannot supply prompt, code, identity or errors.
pub struct Operation {
    context: Context,
    start: Instant,
    kind: OperationKind,
    completed: bool,
    dimensions: Vec<KeyValue>,
    first_chunk: bool,
}
impl Operation {
    pub fn start(parent: Option<&str>, state: Option<&str>, kind: OperationKind) -> Self {
        let parent = remote_context(parent, state).unwrap_or_else(Context::current);
        let tracer = global::tracer_with_scope(
            opentelemetry::InstrumentationScope::builder(SCOPE)
                .with_schema_url(SCHEMA_URL)
                .build(),
        );
        let span = tracer
            .span_builder(kind.name())
            .with_attributes([KeyValue::new("gen_ai.operation.name", kind.name())])
            .start_with_context(&tracer, &parent);
        Self {
            context: parent.with_span(span),
            start: Instant::now(),
            kind,
            completed: false,
            dimensions: Vec::new(),
            first_chunk: false,
        }
    }
    pub fn traceparent(&self) -> Option<String> {
        let span = self.context.span();
        let sc = span.span_context();
        sc.is_valid().then(|| {
            format!(
                "00-{}-{}-{:02x}",
                sc.trace_id(),
                sc.span_id(),
                sc.trace_flags().to_u8()
            )
        })
    }
    pub fn success(&mut self) {
        self.completed = true;
        self.context.span().set_status(Status::Ok);
    }
    pub fn context(&self) -> Context {
        self.context.clone()
    }
    pub fn with_tenant(mut self, tenant: &str) -> Self {
        use sha2::{Digest, Sha256};
        if !tenant.trim().is_empty() && tenant.len() <= 255 && !tenant.chars().any(char::is_control)
        {
            self.dimensions.push(KeyValue::new(
                "agentforge.tenant.id",
                format!("sha256:{:x}", Sha256::digest(tenant.as_bytes())),
            ));
        }
        self
    }
    pub fn with_model(mut self, model: &str) -> Self {
        if !model.is_empty()
            && model.len() <= 128
            && model
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
        {
            self.dimensions
                .push(KeyValue::new("gen_ai.request.model", model.to_owned()));
        }
        self
    }
    pub fn inherit_tenant(&mut self, parent: &Self) {
        self.dimensions.extend(
            parent
                .dimensions
                .iter()
                .filter(|a| a.key.as_str() == "agentforge.tenant.id")
                .cloned(),
        );
    }
    pub fn first_chunk(&mut self) {
        if !self.first_chunk {
            self.first_chunk = true;
            let _active = self.context.clone().attach();
            let mut dimensions = self.dimensions.clone();
            dimensions.push(KeyValue::new("gen_ai.operation.name", self.kind.name()));
            global::meter(SCOPE)
                .f64_histogram("gen_ai.client.operation.time_to_first_chunk")
                .with_unit("s")
                .build()
                .record(self.start.elapsed().as_secs_f64(), &dimensions);
        }
    }
    pub fn tokens(&self, input: u64, output: u64) {
        let _active = self.context.clone().attach();
        for (key, count) in [
            ("gen_ai.usage.input_tokens", input),
            ("gen_ai.usage.output_tokens", output),
        ] {
            self.context
                .span()
                .set_attribute(KeyValue::new(key, i64::try_from(count).unwrap_or(i64::MAX)));
        }
        let counter = global::meter(SCOPE)
            .u64_histogram("gen_ai.client.token.usage")
            .with_unit("{token}")
            .build();
        for (kind, count) in [("input", input), ("output", output)] {
            let mut dimensions = self.dimensions.clone();
            dimensions.push(KeyValue::new("gen_ai.token.type", kind));
            dimensions.push(KeyValue::new("gen_ai.operation.name", self.kind.name()));
            counter.record(count, &dimensions);
        }
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        let _active = self.context.clone().attach();
        let outcome = if self.completed { "ok" } else { "incomplete" };
        if !self.completed {
            self.context
                .span()
                .set_status(Status::error("operation_incomplete"));
        }
        self.context.span().set_attributes(self.dimensions.clone());
        let mut dimensions = self.dimensions.clone();
        dimensions.extend([
            KeyValue::new("gen_ai.operation.name", self.kind.name()),
            KeyValue::new("outcome", outcome),
        ]);
        global::meter(SCOPE)
            .f64_histogram("gen_ai.client.operation.duration")
            .with_unit("s")
            .build()
            .record(self.start.elapsed().as_secs_f64(), &dimensions);
        if let Some(provider) = LOGS.get() {
            let logger = provider.logger(SCOPE);
            let mut record = logger.create_log_record();
            record.set_severity_number(if self.completed {
                Severity::Info
            } else {
                Severity::Warn
            });
            record.set_body("gen_ai.operation.finished".into());
            record.add_attribute("gen_ai.operation.name", self.kind.name());
            record.add_attribute("outcome", outcome);
            let span = self.context.span();
            let sc = span.span_context();
            record.set_trace_context(sc.trace_id(), sc.span_id(), Some(sc.trace_flags()));
            logger.emit(record);
        }
        self.context.span().end();
    }
}
fn remote_context(parent: Option<&str>, state: Option<&str>) -> Option<Context> {
    let parent = parent?;
    if parent.len() != 55 || !parent.is_ascii() {
        return None;
    }
    let parts = parent.split('-').collect::<Vec<_>>();
    if parts.len() != 4
        || parts[0] != "00"
        || parts[1].len() != 32
        || parts[2].len() != 16
        || parts[3].len() != 2
        || !parts.iter().all(|p| {
            p.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return None;
    }
    let trace = TraceId::from_hex(parts[1]).ok()?;
    let span = SpanId::from_hex(parts[2]).ok()?;
    if trace == TraceId::INVALID || span == SpanId::INVALID {
        return None;
    }
    let flags = u8::from_str_radix(parts[3], 16).ok()?;
    if flags > 1 {
        return None;
    }
    let state = state
        .filter(|s| s.len() <= 512)
        .and_then(|s| s.parse::<TraceState>().ok())
        .unwrap_or_default();
    Some(Context::new().with_remote_span_context(SpanContext::new(
        trace,
        span,
        TraceFlags::new(flags),
        true,
        state,
    )))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn propagation_rejects_invalid_and_preserves_parent() {
        let valid = "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01";
        let ctx = remote_context(Some(valid), Some("vendor=value")).unwrap();
        assert_eq!(
            ctx.span().span_context().trace_id().to_string(),
            "0123456789abcdef0123456789abcdef"
        );
        assert_eq!(
            ctx.span().span_context().trace_state().header(),
            "vendor=value"
        );
        for value in [
            "secret",
            "00-00000000000000000000000000000000-0123456789abcdef-01",
            "ff-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
        ] {
            assert!(remote_context(Some(value), None).is_none());
        }
    }
    #[test]
    fn tenant_and_model_dimensions_never_export_raw_tenant_or_prompt() {
        let operation = Operation::start(None, None, OperationKind::Chat)
            .with_tenant("private-tenant-name")
            .with_model("provider/model-v1");
        let labels = format!("{:?}", operation.dimensions);
        assert!(labels.contains("sha256:"));
        assert!(!labels.contains("private-tenant-name"));
        assert!(labels.contains("provider/model-v1"));
        assert!(
            Operation::start(None, None, OperationKind::Chat)
                .with_tenant("\nprivate")
                .with_model("private prompt text")
                .dimensions
                .is_empty()
        );
    }
    #[test]
    fn first_chunk_observation_is_once_only() {
        let mut operation = Operation::start(None, None, OperationKind::Chat);
        assert!(!operation.first_chunk);
        operation.first_chunk();
        operation.first_chunk();
        assert!(operation.first_chunk);
        operation.success();
    }
    #[test]
    fn operation_vocabulary_cannot_capture_content() {
        assert_eq!(OperationKind::Chat.name(), "chat");
        assert_eq!(SEMCONV_VERSION, "1.37.0");
        assert!(remote_context(None, Some("authorization=secret")).is_none());
    }
}

#[cfg(test)]
mod export_tests {
    use super::*;
    use opentelemetry::trace::TracerProvider;
    use opentelemetry_sdk::{
        error::OTelSdkResult,
        trace::{SpanData, SpanExporter},
    };
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    #[derive(Clone, Debug, Default)]
    struct Capture {
        spans: Arc<Mutex<Vec<SpanData>>>,
        shutdowns: Arc<AtomicUsize>,
    }
    impl SpanExporter for Capture {
        async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
            self.spans.lock().unwrap().extend(batch);
            Ok(())
        }
        fn shutdown_with_timeout(&self, _: Duration) -> OTelSdkResult {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    #[test]
    fn bounded_batch_flushes_parented_content_free_spans_once() {
        let capture = Capture::default();
        let processor = BatchSpanProcessor::builder(capture.clone())
            .with_batch_config(
                BatchConfigBuilder::default()
                    .with_max_queue_size(32)
                    .with_max_export_batch_size(8)
                    .with_scheduled_delay(Duration::from_secs(60))
                    .build(),
            )
            .build();
        let provider = SdkTracerProvider::builder()
            .with_span_processor(processor)
            .build();
        let parent = remote_context(
            Some("00-0123456789abcdef0123456789abcdef-0123456789abcdef-01"),
            None,
        )
        .unwrap();
        let tracer = provider.tracer(SCOPE);
        for _ in 0..16 {
            let span = tracer
                .span_builder("chat")
                .with_attributes([KeyValue::new("gen_ai.operation.name", "chat")])
                .start_with_context(&tracer, &parent);
            let mut operation = Operation {
                context: parent.with_span(span),
                kind: OperationKind::Chat,
                start: Instant::now(),
                completed: false,
                dimensions: Vec::new(),
                first_chunk: false,
            };
            operation.success();
            drop(operation);
        }
        provider.shutdown().unwrap();
        assert_eq!(capture.shutdowns.load(Ordering::SeqCst), 1);
        let spans = capture.spans.lock().unwrap();
        assert_eq!(spans.len(), 16);
        for span in spans.iter() {
            assert_eq!(span.parent_span_id.to_string(), "0123456789abcdef");
            assert_eq!(
                span.span_context.trace_id().to_string(),
                "0123456789abcdef0123456789abcdef"
            );
            assert_eq!(span.attributes.len(), 1);
            assert_eq!(span.attributes[0].key.as_str(), "gen_ai.operation.name");
            assert!(span.events.events.is_empty());
        }
    }
    #[test]
    fn incomplete_operation_is_exported_as_error_without_error_text() {
        let capture = Capture::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(capture.clone())
            .build();
        let span = provider.tracer(SCOPE).start("execute_tool");
        drop(Operation {
            context: Context::new().with_span(span),
            kind: OperationKind::ExecuteTool,
            start: Instant::now(),
            completed: false,
            dimensions: Vec::new(),
            first_chunk: false,
        });
        provider.shutdown().unwrap();
        let spans = capture.spans.lock().unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].status, Status::error("operation_incomplete"));
    }
}

fn validate_endpoint(endpoint: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(endpoint).map_err(|_| "otel_endpoint_invalid")?;
    if !["http", "https"].contains(&url.scheme())
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("otel_endpoint_invalid".into());
    }
    Ok(())
}

#[cfg(test)]
mod config_tests {
    use super::*;
    #[test]
    fn collector_configuration_does_not_admit_credentials_or_unbounded_values() {
        for endpoint in ["http://localhost:4318", "https://collector.internal/otlp"] {
            assert!(validate_endpoint(endpoint).is_ok());
        }
        for endpoint in [
            "file:///private/env",
            "https://user:secret@collector/",
            "https://collector/?token=secret",
            "https://collector/#secret",
        ] {
            assert!(validate_endpoint(endpoint).is_err());
        }
    }
}
