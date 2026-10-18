//! Safe exporter failure diagnostics; never format transport errors or endpoints.
use opentelemetry_sdk::{
    Resource,
    error::OTelSdkResult,
    logs::{LogBatch, LogExporter},
    metrics::{Temporality, data::ResourceMetrics, exporter::PushMetricExporter},
    trace::{SpanData, SpanExporter},
};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
static FAILURES: AtomicU64 = AtomicU64::new(0);
fn observe(result: &OTelSdkResult, signal: &'static str) {
    if result.is_err() {
        let count = FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
        opentelemetry::global::meter("agentforge.telemetry")
            .u64_counter("agentforge.otel.export.failures")
            .build()
            .add(1, &[opentelemetry::KeyValue::new("signal", signal)]);
        if count.is_power_of_two() {
            eprintln!("telemetry batch export failed signal={signal} failures={count}");
        }
    }
}
#[derive(Debug)]
pub(super) struct Traces(pub opentelemetry_otlp::SpanExporter);
impl SpanExporter for Traces {
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        let result = self.0.export(batch).await;
        observe(&result, "traces");
        result
    }
    fn set_resource(&mut self, resource: &Resource) {
        self.0.set_resource(resource);
    }
    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.0.shutdown_with_timeout(timeout)
    }
}
#[derive(Debug)]
pub(super) struct Logs(pub opentelemetry_otlp::LogExporter);
impl LogExporter for Logs {
    async fn export(&self, batch: LogBatch<'_>) -> OTelSdkResult {
        let result = self.0.export(batch).await;
        observe(&result, "logs");
        result
    }
    fn set_resource(&mut self, resource: &Resource) {
        self.0.set_resource(resource);
    }
    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.0.shutdown_with_timeout(timeout)
    }
}
#[derive(Debug)]
pub(super) struct Metrics(pub opentelemetry_otlp::MetricExporter);
impl PushMetricExporter for Metrics {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        let result = self.0.export(metrics).await;
        observe(&result, "metrics");
        result
    }
    fn force_flush(&self) -> OTelSdkResult {
        self.0.force_flush()
    }
    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.0.shutdown_with_timeout(timeout)
    }
    fn temporality(&self) -> Temporality {
        self.0.temporality()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_failure_is_counted_without_error_capture() {
        let before = FAILURES.load(Ordering::Relaxed);
        observe(
            &Err(opentelemetry_sdk::error::OTelSdkError::InternalFailure(
                "private exporter diagnostic".into(),
            )),
            "traces",
        );
        assert!(FAILURES.load(Ordering::Relaxed) > before);
    }
}
