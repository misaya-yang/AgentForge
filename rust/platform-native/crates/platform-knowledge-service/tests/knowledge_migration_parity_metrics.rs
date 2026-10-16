//! Process-isolated deltas from the actual native Prometheus registry.
use ai_platform_knowledge_service::migration_knowledge_query::pipeline_metrics as metrics;
fn sample(name: &str, labels: &str) -> f64 {
    let prefix = if labels.is_empty() {
        format!("{name} ")
    } else {
        format!("{name}{{{labels}}} ")
    };
    metrics::render()
        .lines()
        .find_map(|line| line.strip_prefix(&prefix).map(|v| v.parse().unwrap()))
        .unwrap_or(0.0)
}
#[test]
fn pipeline_metrics_preserve_counters_histograms_bounded_labels_and_bad_duration_contracts() {
    let action = r#"action="reembed",outcome="completed""#;
    let duration = r#"mode="scanned",outcome="completed""#;
    let before = sample("kb_ingest_actions_total", action);
    let count = sample("kb_ingest_duration_seconds_count", duration);
    let sum = sample("kb_ingest_duration_seconds_sum", duration);
    metrics::ingest("reembed", "scanned", true, 12.5);
    assert_eq!(sample("kb_ingest_actions_total", action) - before, 1.0);
    assert_eq!(
        sample("kb_ingest_duration_seconds_count", duration) - count,
        1.0
    );
    assert!((sample("kb_ingest_duration_seconds_sum", duration) - sum - 12.5).abs() < 1e-9);
    for (action, expected) in [
        ("REEMBED", "reembed"),
        (" retry ", "retry"),
        ("ingest", "ingest"),
        ("bulk-backfill", "other"),
        ("", "other"),
    ] {
        let labels = format!("action=\"{expected}\",outcome=\"error\"");
        let before = sample("kb_ingest_actions_total", &labels);
        metrics::ingest(action, "auto", false, 1.0);
        assert_eq!(sample("kb_ingest_actions_total", &labels) - before, 1.0);
    }
    let labels = r#"mode="unknown",outcome="error""#;
    let before = sample("kb_ingest_duration_seconds_count", labels);
    metrics::ingest("retry", "text", false, 2.0);
    assert_eq!(
        sample("kb_ingest_duration_seconds_count", labels) - before,
        1.0
    );
    let labels = r#"action="recover",outcome="completed""#;
    let before = sample("kb_ingest_actions_total", labels);
    let hist = r#"mode="auto",outcome="completed""#;
    let count = sample("kb_ingest_duration_seconds_count", hist);
    for invalid in [-1.0, f64::NAN, f64::INFINITY] {
        metrics::ingest("recover", "auto", true, invalid);
    }
    assert_eq!(sample("kb_ingest_actions_total", labels) - before, 3.0);
    assert_eq!(sample("kb_ingest_duration_seconds_count", hist), count);
    for outcome in ["claimed", "contended", "deferred"] {
        let labels = format!("outcome=\"{outcome}\"");
        let before = sample("kb_worker_claims_total", &labels);
        metrics::worker_claim(outcome);
        assert_eq!(sample("kb_worker_claims_total", &labels) - before, 1.0);
    }
    let labels = r#"outcome="deferred""#;
    let before = sample("kb_worker_claims_total", labels);
    metrics::worker_claim("connection reset by peer");
    metrics::worker_claim("");
    assert_eq!(sample("kb_worker_claims_total", labels) - before, 2.0);
    let recovered = sample("kb_stuck_documents_recovered_total", "");
    let requeued = sample("kb_stuck_documents_requeued_total", "");
    metrics::recovered(3, 2);
    assert_eq!(
        sample("kb_stuck_documents_recovered_total", "") - recovered,
        3.0
    );
    assert_eq!(
        sample("kb_stuck_documents_requeued_total", "") - requeued,
        2.0
    );
    for outcome in ["ok", "error", "timeout"] {
        let labels = format!("outcome=\"{outcome}\"");
        let before = sample("kb_embedding_calls_total", &labels);
        metrics::embedding(outcome, 0.3);
        assert_eq!(sample("kb_embedding_calls_total", &labels) - before, 1.0);
    }
    let labels = r#"outcome="error""#;
    let before = sample("kb_embedding_calls_total", labels);
    metrics::embedding("rate limit from provider", f64::NAN);
    metrics::embedding("", f64::NAN);
    assert_eq!(sample("kb_embedding_calls_total", labels) - before, 2.0);
    let labels = r#"outcome="ok""#;
    let before = sample("kb_embedding_duration_seconds_count", labels);
    metrics::embedding("ok", f64::NAN);
    assert_eq!(
        sample("kb_embedding_duration_seconds_count", labels),
        before
    );
    metrics::embedding("ok", 0.05);
    assert_eq!(
        sample("kb_embedding_duration_seconds_count", labels) - before,
        1.0
    );
    let body = metrics::render();
    for name in [
        "kb_ingest_duration_seconds",
        "kb_ingest_actions_total",
        "kb_worker_claims_total",
        "kb_stuck_documents_recovered_total",
        "kb_embedding_calls_total",
        "kb_embedding_duration_seconds",
    ] {
        assert!(body.contains(name));
    }
    assert!(!body.contains("connection reset by peer"));
    assert!(!body.contains("rate limit from provider"));
    assert!(!body.contains("bulk-backfill"));
}
