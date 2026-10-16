//! Gateway-owned collector translated from services/metrics/collector.py.
//! Label insertion order, cumulative buckets, retained last 10,000 samples,
//! and the source's empty-label percentile behavior are preserved.
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};
pub const DEFAULT_BUCKETS: &[f64] = &[
    5., 10., 25., 50., 75., 100., 250., 500., 750., 1000., 2500., 5000., 7500., 10000.,
];
#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Counter,
    Gauge,
    Histogram,
}
#[derive(Clone)]
pub struct MetricValue {
    pub value: f64,
    pub timestamp: f64,
    pub labels: Vec<(String, String)>,
}
#[derive(Default)]
struct Row {
    key: Vec<String>,
    value: f64,
    count: u64,
    buckets: Vec<u64>,
    samples: Vec<f64>,
}
pub struct Metric {
    pub name: String,
    pub description: String,
    pub labels: Vec<String>,
    pub kind: Kind,
    pub buckets: Vec<f64>,
    rows: Mutex<Vec<Row>>,
}
impl Metric {
    pub fn new(
        name: &str,
        description: &str,
        labels: &[&str],
        kind: Kind,
        buckets: &[f64],
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            labels: labels.iter().map(|s| (*s).into()).collect(),
            kind,
            buckets: if buckets.is_empty() {
                DEFAULT_BUCKETS.to_vec()
            } else {
                buckets.to_vec()
            },
            rows: Mutex::new(vec![]),
        }
    }
    fn key(&self, labels: &[(&str, &str)]) -> Vec<String> {
        self.labels
            .iter()
            .map(|name| {
                labels
                    .iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| (*v).to_owned())
                    .unwrap_or_default()
            })
            .collect()
    }
    fn mutate(&self, labels: &[(&str, &str)], f: impl FnOnce(&mut Row)) {
        let key = self.key(labels);
        let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let index = rows.iter().position(|r| r.key == key).unwrap_or_else(|| {
            rows.push(Row {
                key,
                buckets: vec![0; self.buckets.len()],
                ..Default::default()
            });
            rows.len() - 1
        });
        f(&mut rows[index]);
    }
    pub fn inc(&self, value: f64, labels: &[(&str, &str)]) {
        self.mutate(labels, |r| r.value += value);
    }
    pub fn dec(&self, value: f64, labels: &[(&str, &str)]) {
        self.inc(-value, labels);
    }
    pub fn set(&self, value: f64, labels: &[(&str, &str)]) {
        self.mutate(labels, |r| r.value = value);
    }
    pub fn get(&self, labels: &[(&str, &str)]) -> f64 {
        let key = self.key(labels);
        self.rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|r| r.key == key)
            .map(|r| r.value)
            .unwrap_or(0.)
    }
    pub fn observe(&self, value: f64, labels: &[(&str, &str)]) {
        self.mutate(labels, |r| {
            for (i, bound) in self.buckets.iter().enumerate() {
                if value <= *bound {
                    r.buckets[i] += 1;
                }
            }
            r.value += value;
            r.count += 1;
            r.samples.push(value);
            if r.samples.len() > 10000 {
                r.samples.drain(..r.samples.len() - 10000);
            }
        });
    }
    pub fn percentile(&self, p: f64, labels: &[(&str, &str)]) -> Option<f64> {
        let key = self.key(labels);
        let rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let row = rows.iter().find(|r| r.key == key)?;
        let mut samples = row.samples.clone();
        if samples.is_empty() {
            return None;
        }
        samples.sort_by(f64::total_cmp);
        let raw = (samples.len() as f64 * p / 100.) as i64;
        let index = raw.min(samples.len() as i64 - 1);
        let index = if index < 0 {
            samples.len() as i64 + index
        } else {
            index
        };
        usize::try_from(index)
            .ok()
            .and_then(|i| samples.get(i))
            .copied()
    }
    pub fn avg(&self, labels: &[(&str, &str)]) -> Option<f64> {
        let key = self.key(labels);
        let rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let row = rows.iter().find(|r| r.key == key && r.count != 0)?;
        Some(row.value / row.count as f64)
    }
    pub fn collect(&self) -> Vec<MetricValue> {
        self.rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|r| MetricValue {
                value: r.value,
                timestamp: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64(),
                labels: self.labels.iter().cloned().zip(r.key.clone()).collect(),
            })
            .collect()
    }
    fn prometheus(&self) -> Vec<String> {
        let kind = match self.kind {
            Kind::Counter => "counter",
            Kind::Gauge => "gauge",
            Kind::Histogram => "histogram",
        };
        let mut lines = vec![
            format!("# HELP {} {}", self.name, self.description),
            format!("# TYPE {} {kind}", self.name),
        ];
        let rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let labels = |r: &Row| {
            self.labels
                .iter()
                .cloned()
                .zip(r.key.clone())
                .collect::<Vec<_>>()
        };
        if self.kind == Kind::Histogram {
            for r in rows.iter() {
                for (i, bucket) in self.buckets.iter().enumerate() {
                    let mut l = labels(r);
                    l.push(("le".into(), bucket.to_string()));
                    lines.push(format!(
                        "{}_bucket{} {}",
                        self.name,
                        format_labels(&l),
                        r.buckets[i]
                    ));
                }
            }
            for r in rows.iter() {
                lines.push(format!(
                    "{}_sum{} {:?}",
                    self.name,
                    format_labels(&labels(r)),
                    r.value
                ));
            }
            for r in rows.iter() {
                lines.push(format!(
                    "{}_count{} {}",
                    self.name,
                    format_labels(&labels(r)),
                    r.count
                ));
            }
        } else {
            for r in rows.iter() {
                lines.push(format!(
                    "{}{} {:?}",
                    self.name,
                    format_labels(&labels(r)),
                    r.value
                ));
            }
        }
        lines
    }
}
fn format_labels(labels: &[(String, String)]) -> String {
    if labels.is_empty() {
        String::new()
    } else {
        format!(
            "{{{}}}",
            labels
                .iter()
                .map(|(k, v)| format!("{k}=\"{v}\""))
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}
pub struct MetricsCollector {
    metrics: Mutex<Vec<Arc<Metric>>>,
}
impl MetricsCollector {
    pub fn new() -> Self {
        let this = Self {
            metrics: Mutex::new(vec![]),
        };
        for (name, description, labels, kind) in DEFINITIONS {
            this.register(Metric::new(name, description, labels, *kind, &[]));
        }
        this.register(Metric::new(
            "gateway_up",
            "Gateway metrics endpoint availability",
            &[],
            Kind::Gauge,
            &[],
        ))
        .set(1., &[]);
        this
    }
    pub fn register(&self, metric: Metric) -> Arc<Metric> {
        let metric = Arc::new(metric);
        let mut metrics = self.metrics.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(index) = metrics
            .iter()
            .position(|m| m.name == metric.name && m.kind == metric.kind)
        {
            metrics[index] = metric.clone();
        } else {
            metrics.push(metric.clone());
        }
        metric
    }
    pub fn metric(&self, name: &str) -> Option<Arc<Metric>> {
        self.metrics
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|m| m.name == name)
            .cloned()
    }
    pub fn record_request(
        &self,
        method: &str,
        path: &str,
        status: u16,
        duration_ms: f64,
        service: &str,
    ) {
        let status = status.to_string();
        let labels = [
            ("method", method),
            ("path", path),
            ("status_code", status.as_str()),
            ("service_id", service),
        ];
        if let Some(m) = self.metric("gateway_requests_total") {
            m.inc(1., &labels);
        }
        if let Some(m) = self.metric("gateway_request_duration_ms") {
            m.observe(duration_ms, &labels);
        }
        if status.parse::<u16>().unwrap_or(0) >= 400
            && let Some(m) = self.metric("gateway_errors_total")
        {
            m.inc(
                1.,
                &[
                    ("method", method),
                    ("path", path),
                    ("error_code", &status),
                    ("service_id", service),
                ],
            );
        }
    }
    pub fn stats(&self) -> Value {
        let total = |name| {
            self.metric(name)
                .map(|m| m.collect().iter().map(|v| v.value).sum::<f64>())
                .unwrap_or(0.)
        };
        let mut by_status = serde_json::Map::new();
        if let Some(m) = self.metric("gateway_requests_total") {
            for row in m.collect() {
                if let Some((_, status)) = row.labels.get(2) {
                    by_status.insert(status.clone(), json!(row.value));
                }
            }
        }
        let latency = self.metric("gateway_request_duration_ms");
        json!({"requests":{"total":total("gateway_requests_total"),"by_status":by_status},"latency":{"p50":latency.as_ref().and_then(|m|m.percentile(50.,&[])),"p95":latency.as_ref().and_then(|m|m.percentile(95.,&[])),"p99":latency.as_ref().and_then(|m|m.percentile(99.,&[])),"avg":latency.as_ref().and_then(|m|m.avg(&[]))},"errors":{"total":total("gateway_errors_total")},"rate_limits":{"total":total("gateway_rate_limit_triggered_total"),"decisions_total":total("gateway_rate_limit_decision_total")}})
    }
    pub fn to_prometheus(&self) -> String {
        let metrics = self.metrics.lock().unwrap_or_else(|e| e.into_inner());
        let mut lines = Vec::new();
        for kind in [Kind::Counter, Kind::Gauge, Kind::Histogram] {
            for metric in metrics.iter().filter(|m| m.kind == kind) {
                lines.extend(metric.prometheus());
            }
        }
        lines.join("\n")
    }
    pub fn register_service_metrics(&self) -> Vec<Arc<Metric>> {
        vec![
            self.register(Metric::new(
                "service_call_duration_seconds",
                "Internal service call duration in seconds",
                &["service", "method", "status"],
                Kind::Histogram,
                &[0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1., 2.5, 5., 10.],
            )),
            self.register(Metric::new(
                "service_call_total",
                "Internal service calls",
                &["service", "method", "status"],
                Kind::Counter,
                &[],
            )),
            self.register(Metric::new(
                "service_call_inflight",
                "Internal service calls in flight",
                &["service"],
                Kind::Gauge,
                &[],
            )),
        ]
    }
}
impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}
pub fn global() -> Arc<MetricsCollector> {
    static GLOBAL: OnceLock<Arc<MetricsCollector>> = OnceLock::new();
    GLOBAL
        .get_or_init(|| Arc::new(MetricsCollector::new()))
        .clone()
}

const DEFINITIONS: &[(&str, &str, &[&str], Kind)] = &[
    (
        "admission_rejected_total",
        "Capacity admission rejections",
        &["service", "reason"],
        Kind::Counter,
    ),
    (
        "gateway_requests_total",
        "Total number of requests",
        &["method", "path", "status_code", "service_id"],
        Kind::Counter,
    ),
    (
        "gateway_request_duration_ms",
        "Request duration in milliseconds",
        &["method", "path", "service_id"],
        Kind::Histogram,
    ),
    (
        "gateway_errors_total",
        "Total number of errors",
        &["method", "path", "error_code", "service_id"],
        Kind::Counter,
    ),
    (
        "gateway_rate_limit_triggered_total",
        "Total number of rate limit triggers",
        &["dimension", "service_id"],
        Kind::Counter,
    ),
    (
        "gateway_rate_limit_decision_total",
        "Total number of rate limit decisions",
        &["dimension", "service_id", "result"],
        Kind::Counter,
    ),
    (
        "gateway_circuit_breaker_state",
        "Circuit breaker state (0=closed, 1=open, 2=half-open)",
        &["service_id"],
        Kind::Gauge,
    ),
    (
        "gateway_active_connections",
        "Number of active connections",
        &["service_id"],
        Kind::Gauge,
    ),
    (
        "gateway_billing_flush_failures_total",
        "Billing buffer flush failures by stage",
        &["stage", "error_type"],
        Kind::Counter,
    ),
    (
        "gateway_billing_records_dropped_total",
        "Billing records permanently dropped after retries / DLQ",
        &["reason"],
        Kind::Counter,
    ),
    (
        "gateway_telemetry_background_backlog",
        "Non-critical request telemetry writes currently in flight",
        &[],
        Kind::Gauge,
    ),
    (
        "gateway_telemetry_records_dropped_total",
        "Non-critical request telemetry records dropped before recording",
        &["reason"],
        Kind::Counter,
    ),
    (
        "gateway_telemetry_record_failures_total",
        "Request telemetry writes that failed or exceeded their deadline",
        &["priority", "reason"],
        Kind::Counter,
    ),
];

/// Source admission counter with finite reason vocabulary and capped service cardinality.
pub struct AdmissionRejectionMetrics {
    counter: Arc<Metric>,
    services: Mutex<std::collections::BTreeSet<String>>,
}
impl AdmissionRejectionMetrics {
    pub fn new(collector: &MetricsCollector) -> Self {
        Self {
            counter: collector
                .metric("admission_rejected_total")
                .expect("registered admission counter"),
            services: Mutex::new(Default::default()),
        }
    }
}
impl ai_platform_gateway_foundation::capacity_admission::AdmissionRejectionObserver
    for AdmissionRejectionMetrics
{
    fn rejected(&self, service: &str, reason: &str) {
        let reason = match reason {
            "GATEWAY_CAPACITY_EXHAUSTED"
            | "GATEWAY_TENANT_CAPACITY_EXHAUSTED"
            | "GATEWAY_CAPACITY_DEGRADED"
            | "GATEWAY_CAPACITY_LEASE_LOST"
            | "GATEWAY_LOAD_SHED" => reason,
            _ => "other",
        };
        let mut services = self.services.lock().unwrap_or_else(|e| e.into_inner());
        let service =
            if service.len() > 128 || (!services.contains(service) && services.len() >= 1024) {
                "other"
            } else {
                services.insert(service.to_owned());
                service
            };
        self.counter
            .inc(1., &[("service", service), ("reason", reason)]);
    }
}
