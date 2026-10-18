//! Offline observability deployment contracts, not Collector/promtool validation.
use serde_json::{Value, json};
use std::{fs, path::Path};

type Result<T> = std::result::Result<T, String>;
fn ensure(ok: bool, message: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(message.into()) }
}
fn untag(value: serde_yaml::Value) -> serde_yaml::Value {
    use serde_yaml::Value as Y;
    match value {
        Y::Tagged(tagged) => untag(tagged.value),
        Y::Sequence(items) => Y::Sequence(items.into_iter().map(untag).collect()),
        Y::Mapping(items) => Y::Mapping(
            items
                .into_iter()
                .map(|(k, v)| (untag(k), untag(v)))
                .collect(),
        ),
        value => value,
    }
}
fn yaml(path: &Path) -> Result<Value> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let value: serde_yaml::Value = serde_yaml::from_str(&text).map_err(|e| e.to_string())?;
    serde_json::to_value(untag(value)).map_err(|e| e.to_string())
}
fn array<'a>(value: &'a Value, message: &str) -> Result<&'a Vec<Value>> {
    value.as_array().ok_or_else(|| message.into())
}
struct Inputs {
    compose: Value,
    collector: Value,
    prometheus: Value,
    rules: Value,
    datasources: Value,
    dashboard: Value,
    compose_text: String,
    metrics_source: String,
}
impl Inputs {
    fn load(root: &Path) -> Result<Self> {
        let dir = root.join("docker/monitoring");
        Ok(Self {
            compose: yaml(&dir.join("docker-compose.monitoring.yml"))?,
            collector: yaml(&dir.join("otel-collector-config.yaml"))?,
            prometheus: yaml(&dir.join("prometheus.yml"))?,
            rules: yaml(&dir.join("alert-rules.yml"))?,
            datasources: yaml(&dir.join("grafana/provisioning/datasources/datasources.yaml"))?,
            dashboard: serde_json::from_slice(
                &fs::read(dir.join("grafana/dashboards/ai-gateway-overview.json"))
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?,
            compose_text: fs::read_to_string(dir.join("docker-compose.monitoring.yml"))
                .map_err(|e| e.to_string())?,
            metrics_source: fs::read_to_string(root.join(
                "rust/platform-native/crates/platform-gateway/src/migration_host/metrics.rs",
            ))
            .map_err(|e| e.to_string())?,
        })
    }
    fn verify(&self) -> Result<()> {
        let services = self.compose["services"]
            .as_object()
            .ok_or("services object missing")?;
        for forbidden in ["postgres", "redis", "frontend", "jaeger"] {
            ensure(
                !services.contains_key(forbidden),
                "duplicate infrastructure",
            )?;
        }
        let backends: Vec<_> = services
            .iter()
            .filter(|(_, s)| {
                let image = s["image"].as_str().unwrap_or("");
                image.contains("tempo") || image.contains("jaeger")
            })
            .map(|(name, _)| name.as_str())
            .collect();
        ensure(backends == ["tempo"], "exactly one trace backend required")?;
        for name in [
            "tempo",
            "otel-collector",
            "prometheus",
            "grafana",
            "loki",
            "alertmanager",
        ] {
            let s = &self.compose["services"][name];
            ensure(
                s["profiles"] == json!(["observability"]),
                "monitoring must be opt-in",
            )?;
            let limit = s["mem_limit"]
                .as_str()
                .and_then(|s| s.strip_suffix('m'))
                .and_then(|s| s.parse::<u32>().ok());
            ensure(
                limit.is_some_and(|n| n > 0 && n <= 256),
                "bounded memory required",
            )?;
            if let Some(ports) = s.get("ports") {
                ensure(
                    array(ports, "ports must be array")?
                        .iter()
                        .all(|p| p.as_str().is_some_and(|s| s.starts_with("127.0.0.1:"))),
                    "public telemetry port",
                )?;
            }
            let image = s["image"].as_str().ok_or("image missing")?;
            ensure(
                image.contains(':') && !image.contains(":latest"),
                "version pin required",
            )?;
        }
        ensure(
            self.compose["services"]["tempo"]["ports"] == json!([])
                && self.compose_text.contains("ports: !reset []"),
            "inherited Tempo ports must be reset",
        )?;
        let grafana = &self.compose["services"]["grafana"]["environment"];
        ensure(
            grafana["GF_SECURITY_ADMIN_PASSWORD__FILE"] == "/run/secrets/grafana-admin",
            "Grafana file secret required",
        )?;
        ensure(
            grafana.get("GF_SECURITY_ADMIN_PASSWORD").is_none(),
            "inline Grafana password forbidden",
        )?;
        for (names, endpoint) in [
            (
                &["gateway", "knowledge-service", "knowledge-worker"][..],
                "http://otel-collector:4318",
            ),
            (
                &["agent-runtime", "agent-capability-worker"][..],
                "http://otel-collector:4318",
            ),
        ] {
            for name in names {
                ensure(
                    self.compose["services"][name]["environment"]["OTEL_EXPORTER_OTLP_ENDPOINT"]
                        == endpoint,
                    "service OTLP endpoint mismatch",
                )?;
            }
        }
        let pipelines = self.collector["service"]["pipelines"]
            .as_object()
            .ok_or("pipelines missing")?;
        ensure(
            pipelines.len() == 3
                && ["traces", "metrics", "logs"]
                    .iter()
                    .all(|s| pipelines.contains_key(*s)),
            "all three signals required",
        )?;
        ensure(
            pipelines["traces"]["exporters"] == json!(["otlp/tempo"]),
            "single trace sink required",
        )?;
        let exporters = self.collector["exporters"]
            .as_object()
            .ok_or("exporters missing")?;
        ensure(
            exporters.len() == 3
                && ["otlp/tempo", "otlphttp/loki", "prometheus"]
                    .iter()
                    .all(|s| exporters.contains_key(*s)),
            "unexpected exporter",
        )?;
        for pipeline in pipelines.values() {
            let processors = array(&pipeline["processors"], "processors missing")?;
            ensure(
                processors.first() == Some(&json!("memory_limiter"))
                    && processors.contains(&json!("transform/privacy")),
                "memory/privacy processing required",
            )?;
        }
        let privacy = self.collector["processors"]["transform/privacy"].to_string();
        ensure(
            privacy.contains("keep_keys(resource.attributes")
                && privacy.contains("keep_keys(attributes")
                && privacy.contains("[redacted]")
                && !privacy.to_lowercase().contains("authorization"),
            "content privacy policy missing",
        )?;
        let jobs = array(&self.prometheus["scrape_configs"], "scrapes missing")?;
        let gateway = jobs
            .iter()
            .find(|j| j["job_name"] == "gateway")
            .ok_or("Gateway scrape missing")?;
        ensure(
            gateway["static_configs"][0]["targets"] == json!(["gateway:8080"]),
            "Gateway endpoint mismatch",
        )?;
        ensure(
            gateway["authorization"]["credentials_file"] == "/run/secrets/metrics-token",
            "metrics credential required",
        )?;
        if let Some(relabels) = gateway.get("metric_relabel_configs") {
            ensure(
                !array(relabels, "relabels invalid")?
                    .iter()
                    .any(|r| r["action"] == "labeldrop"),
                "label removal can collide source series",
            )?;
        }
        ensure(
            !jobs.iter().any(|j| j.to_string().contains(":6379")),
            "Redis is not a Prometheus endpoint",
        )?;
        let otlp = jobs
            .iter()
            .find(|j| j["job_name"] == "otlp")
            .ok_or("OTLP scrape missing")?;
        ensure(
            otlp["static_configs"][0]["targets"] == json!(["otel-collector:8889"]),
            "OTLP scrape mismatch",
        )?;
        let groups = array(&self.rules["groups"], "rule groups missing")?;
        let rules: Vec<_> = groups
            .iter()
            .map(|g| array(&g["rules"], "rules missing"))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        for kind in ["error", "slow"] {
            for window in ["5m", "30m", "1h", "6h"] {
                let record = format!("gateway:slo_{kind}_ratio:{window}");
                ensure(
                    rules.iter().any(|r| r["record"] == record),
                    "SLO recording window missing",
                )?;
            }
            for suffix in ["Fast", "Slow"] {
                let title = if kind == "error" { "Error" } else { "Slow" };
                let name = format!("Gateway{title}BudgetBurn{suffix}");
                let alert = rules
                    .iter()
                    .find(|r| r["alert"] == name)
                    .ok_or("SLO burn alert missing")?;
                ensure(
                    alert["expr"].as_str().is_some_and(|s| s.contains(" and "))
                        && alert["labels"]["owner"] == "gateway",
                    "paired SLO windows/owner required",
                )?;
            }
        }
        ensure(
            rules
                .iter()
                .any(|r| r["alert"] == "GatewaySloTelemetryAbsent"),
            "missing telemetry must alert",
        )?;
        let datasources = array(&self.datasources["datasources"], "datasources missing")?;
        ensure(
            datasources.len() == 3
                && ["tempo", "loki", "prometheus"]
                    .iter()
                    .all(|typ| datasources.iter().any(|d| d["type"] == *typ)),
            "datasources mismatch",
        )?;
        ensure(
            array(&self.dashboard["panels"], "panels missing")?.len() >= 6
                && self.dashboard["editable"] == false,
            "provisioned dashboard required",
        )?;
        for metric in [
            "gateway_requests_total",
            "gateway_request_duration_ms",
            "admission_rejected_total",
        ] {
            ensure(
                self.metrics_source.contains(metric),
                "metric absent from production source",
            )?;
        }
        Ok(())
    }
}

pub fn check(root: &Path) -> Result<()> {
    Inputs::load(root)?.verify()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn inputs() -> Inputs {
        Inputs::load(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../..")
                .as_path(),
        )
        .unwrap()
    }
    fn rejects(change: impl FnOnce(&mut Inputs)) {
        let mut fixture = inputs();
        fixture
            .verify()
            .expect("current source must pass before mutation");
        change(&mut fixture);
        assert!(fixture.verify().is_err());
    }
    #[test]
    fn current_contract() {
        inputs().verify().unwrap();
    }
    #[test]
    fn duplicate_backend() {
        rejects(|f| {
            f.compose["services"]["jaeger"] = json!({"image":"jaegertracing/all-in-one:1.51"})
        });
    }
    #[test]
    fn default_enabled() {
        rejects(|f| f.compose["services"]["prometheus"]["profiles"] = json!([]));
    }
    #[test]
    fn public_ingest() {
        rejects(|f| f.compose["services"]["otel-collector"]["ports"] = json!(["4317:4317"]));
    }
    #[test]
    fn missing_metrics_auth() {
        rejects(|f| {
            f.prometheus["scrape_configs"][0]["authorization"]
                .as_object_mut()
                .unwrap()
                .remove("credentials_file");
        });
    }
    #[test]
    fn unredacted_pipeline() {
        rejects(|f| {
            f.collector["service"]["pipelines"]["logs"]["processors"] =
                json!(["memory_limiter", "batch"])
        });
    }
    #[test]
    fn single_window_alert() {
        rejects(|f| f.rules["groups"][0]["rules"][8]["expr"] = json!("vector(1)"));
    }
    #[test]
    fn missing_signal() {
        rejects(|f| {
            f.collector["service"]["pipelines"]
                .as_object_mut()
                .unwrap()
                .remove("logs");
        });
    }
}
