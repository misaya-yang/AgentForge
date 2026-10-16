//! Native process-wide bounded-label Prometheus registry. Recording is best effort.
//! No document, tenant, provider, query text or error payload is retained.
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
};
const RETRIEVE: &[f64] = &[0.05, 0.1, 0.25, 0.5, 1., 2., 3., 5., 10.];
const EMBED: &[f64] = &[0.05, 0.1, 0.25, 0.5, 1., 2., 5., 10., 30.];
const INGEST: &[f64] = &[1., 5., 15., 30., 60., 120., 300., 600., 1800., 3600.];
#[derive(Default)]
struct Registry {
    counters: BTreeMap<(&'static str, String), u64>,
    histograms: BTreeMap<(&'static str, String), Histogram>,
}
struct Histogram {
    buckets: &'static [f64],
    counts: Vec<u64>,
    count: u64,
    sum: f64,
}
static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
fn bounded<'a>(raw: &str, allowed: &[&'a str], fallback: &'a str) -> &'a str {
    let s = raw.trim().to_ascii_lowercase();
    allowed
        .iter()
        .copied()
        .find(|v| *v == s)
        .unwrap_or(fallback)
}
fn record(
    counter: &'static str,
    labels: String,
    histogram: Option<(&'static str, String, &'static [f64], f64)>,
    amount: u64,
) {
    if let Ok(mut r) = REGISTRY.get_or_init(Default::default).lock() {
        *r.counters.entry((counter, labels)).or_default() += amount;
        if let Some((name, labels, buckets, value)) = histogram {
            if value.is_finite() && value >= 0. {
                let h = r
                    .histograms
                    .entry((name, labels))
                    .or_insert_with(|| Histogram {
                        buckets,
                        counts: vec![0; buckets.len()],
                        count: 0,
                        sum: 0.,
                    });
                h.count += 1;
                h.sum += value;
                for (bound, count) in h.buckets.iter().zip(h.counts.iter_mut()) {
                    if value <= *bound {
                        *count += 1;
                    }
                }
            }
        }
    }
}
pub fn retrieval(mode: &str, cache_hit: bool, seconds: f64) {
    let mode = if mode.trim().eq_ignore_ascii_case("vector") {
        "dense"
    } else {
        bounded(mode, &["bm25", "dense", "hybrid", "keyword"], "other")
    };
    record(
        "kb_retrievals_total",
        format!(
            "mode=\"{mode}\",cache=\"{}\"",
            if cache_hit { "hit" } else { "miss" }
        ),
        Some((
            "kb_retrieve_latency_seconds",
            format!("mode=\"{mode}\""),
            RETRIEVE,
            seconds,
        )),
        1,
    );
}
pub fn rerank_degraded(reason: &str) {
    let r = bounded(reason, &["budget_exhausted", "error", "timeout"], "error");
    record(
        "kb_rerank_degraded_total",
        format!("reason=\"{r}\""),
        None,
        1,
    );
}
pub fn bm25_readiness(outcome: &str) {
    let o = bounded(outcome, &["hit", "miss", "failure"], "failure");
    record(
        "kb_bm25_v2_readiness_total",
        format!("outcome=\"{o}\""),
        None,
        1,
    );
}
pub fn worker_claim(outcome: &str) {
    let o = bounded(outcome, &["claimed", "contended", "deferred"], "deferred");
    record(
        "kb_worker_claims_total",
        format!("outcome=\"{o}\""),
        None,
        1,
    );
}
pub fn ingest(action: &str, mode: &str, success: bool, seconds: f64) {
    let a = bounded(
        action,
        &["ingest", "reprocess", "reembed", "recover", "retry"],
        "other",
    );
    let m = bounded(
        mode,
        &["text_only", "scanned", "multimodal", "auto"],
        "unknown",
    );
    let o = if success { "completed" } else { "error" };
    record(
        "kb_ingest_actions_total",
        format!("action=\"{a}\",outcome=\"{o}\""),
        Some((
            "kb_ingest_duration_seconds",
            format!("mode=\"{m}\",outcome=\"{o}\""),
            INGEST,
            seconds,
        )),
        1,
    );
}
pub fn recovered(recovered: u64, requeued: u64) {
    record(
        "kb_stuck_documents_recovered_total",
        String::new(),
        None,
        recovered,
    );
    record(
        "kb_stuck_documents_requeued_total",
        String::new(),
        None,
        requeued,
    );
}
pub fn embedding(outcome: &str, seconds: f64) {
    let o = bounded(outcome, &["ok", "error", "timeout"], "error");
    let labels = format!("outcome=\"{o}\"");
    record(
        "kb_embedding_calls_total",
        labels.clone(),
        Some(("kb_embedding_duration_seconds", labels, EMBED, seconds)),
        1,
    );
}
fn sample(name: &str, labels: &str, value: impl std::fmt::Display) -> String {
    if labels.is_empty() {
        format!("{name} {value}\n")
    } else {
        format!("{name}{{{labels}}} {value}\n")
    }
}
pub fn render() -> String {
    let Ok(r) = REGISTRY.get_or_init(Default::default).lock() else {
        return String::new();
    };
    r.render()
}
impl Registry {
    fn render(&self) -> String {
        let mut out = String::new();
        let mut last = "";
        for ((name, labels), value) in &self.counters {
            if last != *name {
                out.push_str(&format!("# TYPE {name} counter\n"));
                last = name;
            }
            out.push_str(&sample(name, labels, value));
        }
        last = "";
        for ((name, labels), h) in &self.histograms {
            if last != *name {
                out.push_str(&format!("# TYPE {name} histogram\n"));
                last = name;
            }
            for (bound, count) in h.buckets.iter().zip(&h.counts) {
                out.push_str(&sample(
                    &format!("{name}_bucket"),
                    &format!("{labels},le=\"{bound}\""),
                    count,
                ));
            }
            out.push_str(&sample(
                &format!("{name}_bucket"),
                &format!("{labels},le=\"+Inf\""),
                h.count,
            ));
            out.push_str(&sample(&format!("{name}_sum"), labels, h.sum));
            out.push_str(&sample(&format!("{name}_count"), labels, h.count));
        }
        out
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn labels_never_preserve_unknown_input() {
        assert_eq!(
            bounded("tenant-private-secret", &["ok", "error"], "error"),
            "error"
        );
        assert_eq!(bounded(" OK ", &["ok", "error"], "error"), "ok");
    }
    #[test]
    fn histograms_use_cumulative_prometheus_buckets() {
        let mut r = Registry::default();
        r.histograms.insert(
            ("test_seconds", "mode=\"text_only\"".into()),
            Histogram {
                buckets: &[1., 5.],
                counts: vec![1, 2],
                count: 3,
                sum: 10.,
            },
        );
        let text = r.render();
        assert!(text.contains("test_seconds_bucket{mode=\"text_only\",le=\"1\"} 1"));
        assert!(text.contains("le=\"+Inf\"} 3"));
        assert!(text.contains("test_seconds_count{mode=\"text_only\"} 3"));
    }
}
