//! Scrape-time durable queue gauge. Query matches Python count_queued_documents;
//! a failed database refresh keeps the last good observation and never fails a scrape.
use axum::{extract::State, http::header, response::IntoResponse};
use sqlx::PgPool;
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use std::time::Duration;

pub(crate) const QUEUE_DEPTH_SQL: &str = r#"
SELECT count(*) FROM documents AS d
JOIN datasets AS ds ON ds.dataset_id = d.dataset_id
WHERE d.status = 'waiting' AND ds.is_deleted = FALSE
AND NOT (COALESCE(d.metadata, '{}'::jsonb) ? '_document_upload_generation')
AND NOT (COALESCE(d.metadata, '{}'::jsonb) ? '_document_upload_failed')
AND NOT (COALESCE(d.metadata, '{}'::jsonb) ? '_confluence_sync_generation')
AND NOT COALESCE(COALESCE(ds.index_config, '{}'::jsonb)
    -> 'retrieval' ? '_index_deletion_fence', FALSE)
"#;

#[derive(Clone)]
pub(crate) struct Metrics {
    pool: PgPool,
    depth: Arc<AtomicI64>,
}
impl Metrics {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self {
            pool,
            depth: Arc::new(AtomicI64::new(0)),
        }
    }
    fn render(&self) -> String {
        let mut rendered = crate::migration_knowledge_query::pipeline_metrics::render();
        rendered.push_str(&format!(
            "# HELP kb_ingestion_queue_depth Documents dispatchable from the durable ingestion queue.\n# TYPE kb_ingestion_queue_depth gauge\nkb_ingestion_queue_depth {}\n",
            self.depth.load(Ordering::Relaxed)
        ));
        rendered
    }
}
pub(crate) async fn scrape(State(metrics): State<Metrics>) -> impl IntoResponse {
    if let Ok(Ok(depth)) = tokio::time::timeout(
        Duration::from_secs(1),
        sqlx::query_scalar::<_, i64>(QUEUE_DEPTH_SQL).fetch_one(&metrics.pool),
    )
    .await
    {
        if depth >= 0 {
            metrics.depth.store(depth, Ordering::Relaxed);
        }
    }
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        metrics.render(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn queue_gauge_has_canonical_name_and_initial_value() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap();
        let metrics = Metrics::new(pool);
        assert!(metrics.render().ends_with("kb_ingestion_queue_depth 0\n"));
        metrics.depth.store(17, Ordering::Relaxed);
        assert!(metrics.render().ends_with("kb_ingestion_queue_depth 17\n"));
    }
    #[test]
    fn queue_query_excludes_unpublished_and_fenced_documents() {
        for marker in [
            "_document_upload_generation",
            "_document_upload_failed",
            "_confluence_sync_generation",
            "_index_deletion_fence",
        ] {
            assert!(QUEUE_DEPTH_SQL.contains(marker));
        }
        assert!(QUEUE_DEPTH_SQL.contains("ds.is_deleted = FALSE"));
        assert!(QUEUE_DEPTH_SQL.contains("d.status = 'waiting'"));
    }
}
