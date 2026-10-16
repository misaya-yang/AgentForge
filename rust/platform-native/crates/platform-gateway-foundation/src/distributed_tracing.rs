//! Native equivalent of shared tracing bootstrap and non-buffering inbound middleware.
//! Export is optional; a broken exporter never prevents service startup.
use axum::{
    extract::Request,
    http::{HeaderMap, HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};
use opentelemetry::{
    Context, KeyValue, global,
    metrics::{Counter, Histogram},
    propagation::{Extractor, Injector, TextMapCompositePropagator, TextMapPropagator},
    trace::{FutureExt, SpanKind, Status, TraceContextExt, Tracer},
};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    Resource,
    metrics::SdkMeterProvider,
    propagation::{BaggagePropagator, TraceContextPropagator},
    trace::Sampler,
};
use std::{collections::HashMap, future::Future, sync::OnceLock};
use tracing_subscriber::EnvFilter;

static PROVIDER: OnceLock<SdkTracerProvider> = OnceLock::new();
static METER_PROVIDER: OnceLock<SdkMeterProvider> = OnceLock::new();
static HTTP_METRICS: OnceLock<Histogram<f64>> = OnceLock::new();
static GENAI_TOKEN_USAGE: OnceLock<Counter<u64>> = OnceLock::new();
static GENAI_TTFT: OnceLock<Histogram<f64>> = OnceLock::new();
static GENAI_FAILURES: OnceLock<Counter<u64>> = OnceLock::new();
const MAX_PROPAGATION_HEADER_BYTES: usize = 2048;
pub use opentelemetry_sdk::trace::SdkTracerProvider;
#[derive(Clone)]
pub struct RequestId(pub String);
/// Call inside the Tokio runtime, before starting any application tasks.
/// Like the source, the explicit root ratio is 1.0; inherited sampling wins.
pub fn initialize(service_name: &'static str) -> SdkTracerProvider {
    initialize_with_options(service_name, TracingOptions::default())
}
/// Explicit endpoint (including empty to disable export) takes precedence over
/// the environment. Attributes are native OpenTelemetry values, not stringified.
#[derive(Clone, Default)]
pub struct TracingOptions {
    pub otlp_endpoint: Option<String>,
    pub otlp_metrics_endpoint: Option<String>,
    pub resource_attributes: Vec<KeyValue>,
}
impl TracingOptions {
    pub fn resolved_endpoint(&self) -> String {
        self.otlp_endpoint.clone().unwrap_or_else(|| {
            std::env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
                .or_else(|_| std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT"))
                .unwrap_or_default()
        })
    }
    pub fn resolved_metrics_endpoint(&self) -> String {
        self.otlp_metrics_endpoint
            .clone()
            .or_else(|| self.otlp_endpoint.clone())
            .or_else(|| std::env::var("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT").ok())
            .unwrap_or_else(|| std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").unwrap_or_default())
    }
    pub fn resource(&self, service_name: &str) -> Resource {
        Resource::builder()
            .with_attribute(KeyValue::new("service.name", service_name.to_owned()))
            .with_attributes(self.resource_attributes.clone())
            .build()
    }
}
/// Process-wide, first successful initialization wins, matching `initialize`.
/// A malformed exporter configuration remains nonfatal to service startup.
pub fn initialize_with_options(service_name: &str, options: TracingOptions) -> SdkTracerProvider {
    PROVIDER
        .get_or_init(|| {
            install_log_subscriber();
            let mut builder = SdkTracerProvider::builder()
                .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
                    1.0,
                ))))
                .with_resource(options.resource(service_name));
            let endpoint = options.resolved_endpoint();
            if !endpoint.is_empty() {
                match opentelemetry_otlp::SpanExporter::builder()
                    .with_tonic()
                    .with_endpoint(endpoint)
                    .build()
                {
                    Ok(exporter) => builder = builder.with_batch_exporter(exporter),
                    Err(_) => eprintln!(
                        "{service_name} tracing exporter unavailable; retaining in-process spans"
                    ),
                }
            }
            let provider = builder.build();
            let meter_provider = build_meter_provider(service_name, &options);
            global::set_meter_provider(meter_provider.clone());
            let _ = METER_PROVIDER.set(meter_provider);
            global::set_text_map_propagator(w3c_propagator());
            global::set_tracer_provider(provider.clone());
            provider
        })
        .clone()
}

/// Keep first-party `tracing` diagnostics visible without replacing a
/// subscriber installed by an embedding host. The same process-wide provider
/// OnceLock makes this a single initialization attempt per service process.
fn install_log_subscriber() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .json()
        .with_current_span(false)
        .with_span_list(false)
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

fn w3c_propagator() -> TextMapCompositePropagator {
    TextMapCompositePropagator::new(vec![
        Box::new(BaggagePropagator::new()),
        Box::new(TraceContextPropagator::new()),
    ])
}

fn build_meter_provider(service_name: &str, options: &TracingOptions) -> SdkMeterProvider {
    let endpoint = options.resolved_metrics_endpoint();
    let mut builder = SdkMeterProvider::builder().with_resource(options.resource(service_name));
    if !endpoint.is_empty() {
        match opentelemetry_otlp::MetricExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()
        {
            Ok(exporter) => builder = builder.with_periodic_exporter(exporter),
            Err(_) => eprintln!(
                "{service_name} OTLP metrics exporter unavailable; retaining scrape metrics"
            ),
        }
    }
    builder.build()
}

/// Flush both process-wide telemetry providers during graceful shutdown.
pub fn shutdown() {
    if let Some(provider) = PROVIDER.get() {
        let _ = provider.shutdown();
    }
    if let Some(provider) = METER_PROVIDER.get() {
        let _ = provider.shutdown();
    }
}

struct Headers<'a>(&'a HeaderMap);
impl Extractor for Headers<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        let mut values = self.0.get_all(key).iter();
        let value = values.next()?.to_str().ok()?;
        if values.next().is_some() || value.len() > MAX_PROPAGATION_HEADER_BYTES {
            return None;
        }
        Some(value)
    }
    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|key| key.as_str()).collect()
    }
}
/// Explicit equivalent of request.state traceparent/tracestate/baggage. This carrier is
/// generated by the SDK, never trusted as identity or used to modify signed headers.
#[derive(Clone, Debug, Default)]
pub struct TraceCarrier(pub HashMap<String, String>);
impl Injector for TraceCarrier {
    fn set(&mut self, key: &str, value: String) {
        self.0.insert(key.into(), value);
    }
}
pub fn current_carrier() -> TraceCarrier {
    let mut carrier = TraceCarrier::default();
    w3c_propagator().inject_context(&Context::current(), &mut carrier);
    if carrier
        .0
        .get("baggage")
        .is_some_and(|value| value.len() > MAX_PROPAGATION_HEADER_BYTES)
    {
        carrier.0.remove("baggage");
    }
    carrier
}

/// Replace caller-supplied correlation headers with the current span before a
/// trusted internal HTTP or RPC hop. The propagated baggage value is bounded.
pub fn inject_current_context(headers: &mut HeaderMap) {
    inject_carrier(headers, true);
}

/// External provider calls carry trace context only. Caller baggage stays
/// inside the platform service graph and is not disclosed to provider APIs.
pub fn inject_current_trace_context(headers: &mut HeaderMap) {
    inject_carrier(headers, false);
}

/// Run an outbound client operation in a child span so the remote receiver's
/// extracted parent is the concrete Gateway HTTP/RPC call.
pub async fn client_span<F: Future>(name: &'static str, future: F) -> F::Output {
    let parent = Context::current();
    let tracer = global::tracer("ai_platform.http.client");
    let span = tracer
        .span_builder(name)
        .with_kind(SpanKind::Client)
        .start_with_context(&tracer, &parent);
    let context = parent.with_span(span);
    let output = future.with_context(context.clone()).await;
    context.span().end();
    output
}

fn inject_carrier(headers: &mut HeaderMap, include_baggage: bool) {
    let carrier = current_carrier();
    for name in ["traceparent", "tracestate", "baggage"] {
        headers.remove(name);
        if name == "baggage" && !include_baggage {
            continue;
        }
        let Some(value) = carrier.0.get(name) else {
            continue;
        };
        if value.len() > MAX_PROPAGATION_HEADER_BYTES {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
}

fn http_request_duration() -> &'static Histogram<f64> {
    HTTP_METRICS.get_or_init(|| {
        global::meter("ai_platform.gateway.http")
            .f64_histogram("http.server.request.duration")
            .with_unit("s")
            .build()
    })
}

fn genai_token_usage() -> &'static Counter<u64> {
    GENAI_TOKEN_USAGE.get_or_init(|| {
        global::meter("ai_platform.gen_ai")
            .u64_counter("gen_ai.client.token.usage")
            .with_unit("token")
            .build()
    })
}

fn genai_ttft() -> &'static Histogram<f64> {
    GENAI_TTFT.get_or_init(|| {
        global::meter("ai_platform.gen_ai")
            .f64_histogram("ai_platform.gen_ai.time_to_first_visible_seconds")
            .with_unit("s")
            .build()
    })
}

fn genai_failures() -> &'static Counter<u64> {
    GENAI_FAILURES.get_or_init(|| {
        global::meter("ai_platform.gen_ai")
            .u64_counter("ai_platform.gen_ai.request.failures")
            .with_unit("{request}")
            .build()
    })
}

/// Record model token usage without tenant, user, prompt, or model-id labels.
pub fn record_genai_token_usage(input_tokens: i64, output_tokens: i64) {
    let meter = genai_token_usage();
    meter.add(
        input_tokens.max(0) as u64,
        &[KeyValue::new("gen_ai.token.type", "input")],
    );
    meter.add(
        output_tokens.max(0) as u64,
        &[KeyValue::new("gen_ai.token.type", "output")],
    );
}

/// Record first user-visible output timing. Invalid durations are discarded.
pub fn record_genai_ttft(seconds: f64) {
    if seconds.is_finite() && seconds >= 0.0 {
        genai_ttft().record(seconds, &[]);
    }
}

/// Record one failure using a finite stage label, never provider error text.
pub fn record_genai_failure(stage: &'static str) {
    let stage = match stage {
        "pre_dispatch" | "post_dispatch" => stage,
        _ => "other",
    };
    genai_failures().add(1, &[KeyValue::new("error.type", stage)]);
}

pub async fn inbound(mut request: Request, next: Next) -> Response {
    let propagator = w3c_propagator();
    let parent = propagator.extract(&Headers(request.headers()));
    let tracer = global::tracer("ai_gateway_core.tracing");
    let method = request.method().to_string();
    let started = std::time::Instant::now();
    let path = request.uri().path().to_owned();
    let scheme = request.uri().scheme_str().unwrap_or("http");
    let host = request
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let url = if request.uri().authority().is_some() {
        request.uri().to_string()
    } else {
        format!("{scheme}://{host}{}", request.uri())
    };
    let mut attributes = vec![
        KeyValue::new("http.method", request.method().to_string()),
        KeyValue::new("http.url", url),
        KeyValue::new("http.target", path.clone()),
        KeyValue::new("http.scheme", scheme.to_owned()),
    ];
    if let Some(agent) = request
        .headers()
        .get("user-agent")
        .and_then(|h| h.to_str().ok())
    {
        attributes.push(KeyValue::new("http.user_agent", agent.to_owned()));
    }
    if let Some(context) = request.extensions().get::<RequestId>() {
        attributes.push(KeyValue::new("request.id", context.0.clone()));
    }
    let span = tracer
        .span_builder(format!("{} {path}", request.method()))
        .with_kind(SpanKind::Server)
        .with_attributes(attributes)
        .start_with_context(&tracer, &parent);
    let context = parent.with_span(span);
    let mut carrier = TraceCarrier::default();
    propagator.inject_context(&context, &mut carrier);
    request.extensions_mut().insert(carrier);
    // Context is attached on each poll, not across await on a thread-local guard.
    // The response body is returned untouched, including SSE and large downloads.
    let response = next.run(request).with_context(context.clone()).await;
    context.span().set_attribute(KeyValue::new(
        "http.status_code",
        response.status().as_u16() as i64,
    ));
    let status = response.status().as_u16();
    context
        .span()
        .set_status(if response.status().is_server_error() {
            Status::error("")
        } else {
            Status::Ok
        });
    http_request_duration().record(
        started.elapsed().as_secs_f64(),
        &[
            KeyValue::new("http.request.method", method),
            KeyValue::new("http.response.status_code", status as i64),
        ],
    );
    context.span().end();
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::trace::TracerProvider;
    #[tokio::test]
    async fn inbound_propagates_active_span_and_never_buffers_body() {
        use axum::{Router, body::Body, routing::get};
        use tower::ServiceExt;
        let provider = SdkTracerProvider::builder().build();
        global::set_tracer_provider(provider.clone());
        let app = Router::new()
            .route(
                "/stream",
                get(|request: Request| async move {
                    let carrier = request.extensions().get::<TraceCarrier>().unwrap();
                    assert_eq!(current_carrier().0, carrier.0);
                    assert!(
                        carrier.0["traceparent"]
                            .starts_with("00-4bf92f3577b34da6a3ce929d0e0e4736-")
                    );
                    assert_ne!(&carrier.0["traceparent"][36..52], "00f067aa0ba902b7");
                    assert_eq!(carrier.0["tracestate"], "vendor=value");
                    assert_eq!(
                        carrier.0["baggage"],
                        "ai-platform.request-class=interactive"
                    );
                    Response::builder()
                        .status(503)
                        .body(Body::from_stream(futures_util::stream::pending::<
                            Result<axum::body::Bytes, std::io::Error>,
                        >()))
                        .unwrap()
                }),
            )
            .layer(axum::middleware::from_fn(inbound));
        let request = Request::builder()
            .uri("/stream")
            .header(
                "traceparent",
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            )
            .header("tracestate", "vendor=value")
            .header("baggage", "ai-platform.request-class=interactive")
            .body(Body::empty())
            .unwrap();
        let response =
            tokio::time::timeout(std::time::Duration::from_secs(1), app.oneshot(request))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(response.status().as_u16(), 503);
        drop(response);
        provider.shutdown().unwrap();
    }
    #[test]
    fn parent_trace_and_sampling_survive_sdk_carrier() {
        for sampled in ["00", "01"] {
            let provider = SdkTracerProvider::builder()
                .with_sampler(Sampler::ParentBased(Box::new(Sampler::AlwaysOn)))
                .build();
            let mut headers = HeaderMap::new();
            headers.insert(
                "traceparent",
                format!("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-{sampled}")
                    .parse()
                    .unwrap(),
            );
            headers.insert("tracestate", "vendor=value".parse().unwrap());
            let propagator = TraceContextPropagator::new();
            let parent = propagator.extract(&Headers(&headers));
            let span = provider
                .tracer("test")
                .start_with_context("request", &parent);
            let context = parent.with_span(span);
            let mut carrier = TraceCarrier::default();
            propagator.inject_context(&context, &mut carrier);
            assert!(carrier.0["traceparent"].starts_with("00-4bf92f3577b34da6a3ce929d0e0e4736-"));
            assert!(carrier.0["traceparent"].ends_with(sampled));
            assert_eq!(carrier.0["tracestate"], "vendor=value");
            context.span().end();
            provider.shutdown().unwrap();
        }
    }
}
