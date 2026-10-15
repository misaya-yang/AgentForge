//! Explicit Knowledge lifespan. Core health comes from live probes and actual
//! worker handles; construction and object presence never assert startup success.
use super::{KnowledgeConnections, KnowledgeError, Result, settings::RuntimeRole};
use crate::{
    migration_host::middleware::DrainState, migration_knowledge_ingest as ingest,
    migration_knowledge_query as query,
};
use reqwest::{Client, Url};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
};
struct MigrationRunner {
    stop: watch::Sender<bool>,
    handle: JoinHandle<std::result::Result<(), ()>>,
}
pub struct KnowledgeLifecycle {
    pub(crate) pool: PgPool,
    pub(crate) client: Client,
    pub drain: Arc<DrainState>,
    role: RuntimeRole,
    qdrant_origin: Url,
    qdrant_key: Option<String>,
    qdrant_startup_timeout: Duration,
    replay_redis: Option<redis::aio::ConnectionManager>,
    idempotency_redis: Option<redis::aio::ConnectionManager>,
    document_worker: Option<Mutex<ingest::worker::KnowledgeWorker>>,
    migration_worker: Option<Arc<query::migration_jobs::MigrationWorker>>,
    migration_runner: Mutex<Option<MigrationRunner>>,
    transition: Mutex<()>,
    started: AtomicBool,
    local_semantic: Option<query::embedding::EmbeddingManager>,
    pdf_parser: Option<ingest::parser::DocumentParser>,
    pdf_ready: AtomicBool,
}
impl KnowledgeLifecycle {
    pub(super) fn new(
        connections: KnowledgeConnections,
        role: RuntimeRole,
        client: Client,
        qdrant_origin: Url,
        qdrant_key: Option<String>,
        qdrant_startup_timeout: Duration,
        ingestion: Arc<ingest::IngestionService>,
        worker_config: ingest::worker::WorkerConfig,
        migrations: Arc<query::migration::EmbeddingMigration>,
        drain: Arc<DrainState>,
        local_semantic: Option<query::embedding::EmbeddingManager>,
    ) -> Self {
        let pdf_parser = ingestion.parser.supports_pdf().then(|| ingestion.parser.clone());
        Self {
            pool: connections.database,
            client,
            drain,
            role,
            qdrant_origin,
            qdrant_key,
            qdrant_startup_timeout,
            replay_redis: connections.replay_redis,
            idempotency_redis: connections.idempotency_redis,
            document_worker: role.worker().then(|| {
                Mutex::new(ingest::worker::KnowledgeWorker::new(
                    ingestion,
                    worker_config,
                ))
            }),
            migration_worker: role.worker().then(|| {
                Arc::new(query::migration_jobs::MigrationWorker::new(
                    migrations,
                    uuid::Uuid::new_v4().to_string(),
                ))
            }),
            migration_runner: Mutex::new(None),
            transition: Mutex::new(()),
            started: AtomicBool::new(false),
            local_semantic,
            pdf_parser,
            pdf_ready: AtomicBool::new(false),
        }
    }
    async fn qdrant_ready(&self, timeout: Duration) -> bool {
        let mut url = self.qdrant_origin.clone();
        let Ok(mut path) = url.path_segments_mut() else {
            return false;
        };
        path.pop_if_empty().push("collections");
        drop(path);
        let mut request = self.client.get(url).timeout(timeout);
        if let Some(key) = &self.qdrant_key {
            request = request.header("api-key", key);
        }
        tokio::time::timeout(timeout, async {
            let response = request.send().await.ok()?;
            if !response.status().is_success() {
                return None;
            }
            let value: Value = response.json().await.ok()?;
            value.get("result")?.get("collections")?.as_array()?;
            Some(())
        })
        .await
        .is_ok_and(|value| value.is_some())
    }
    async fn database_ready(&self, timeout: Duration) -> bool {
        matches!(
            tokio::time::timeout(
                timeout,
                sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&self.pool)
            )
            .await,
            Ok(Ok(1))
        )
    }
    pub async fn start(&self) -> Result<()> {
        let _transition = self.transition.lock().await;
        let result = self.start_locked().await;
        if result.is_err() {
            self.started.store(false, Ordering::Release);
            self.pdf_ready.store(false, Ordering::Release);
            if let Some(worker) = &self.document_worker {
                let _ = worker.lock().await.stop().await;
            }
            if let Some(runner) = self.migration_runner.lock().await.take() {
                let _ = runner.stop.send(true);
                let _ = runner.handle.await;
            }
            self.pool.close().await;
        }
        result
    }
    async fn start_locked(&self) -> Result<()> {
        if self.started.load(Ordering::Acquire) {
            return Ok(());
        }
        if self.drain.draining() {
            return Err(KnowledgeError::Startup("already_draining"));
        }
        // Optional PDF absence must not disable text ingestion. When selected,
        // run real bounded extraction before workers or API readiness can pass.
        if let Some(parser) = &self.pdf_parser {
            let parser = parser.clone();
            if !matches!(
                tokio::task::spawn_blocking(move || super::pdf_readiness::probe(&parser)).await,
                Ok(Ok(()))
            ) {
                return Err(KnowledgeError::Startup("configured_pdf_backend_probe"));
            }
            self.pdf_ready.store(true, Ordering::Release);
        }
        for connection in [&self.idempotency_redis, &self.replay_redis]
            .into_iter()
            .flatten()
        {
            let mut connection = connection.clone();
            let command = redis::cmd("PING");
            let probe = command.query_async::<String>(&mut connection);
            if !matches!(tokio::time::timeout(Duration::from_secs(5),probe).await,Ok(Ok(ref value)) if value=="PONG")
            {
                return Err(KnowledgeError::Startup("internal_redis"));
            }
        }
        if !self.database_ready(Duration::from_secs(5)).await {
            return Err(KnowledgeError::Startup("database"));
        }
        if !self.qdrant_ready(self.qdrant_startup_timeout).await {
            return Err(KnowledgeError::Startup("qdrant"));
        }
        if let Some(embeddings) = &self.local_semantic {
            if !matches!(
                tokio::time::timeout(
                    Duration::from_secs(60),
                    embeddings.probe_local_semantic(true)
                )
                .await,
                Ok(Ok(()))
            ) {
                return Err(KnowledgeError::Startup("local_semantic_model"));
            }
        }
        if let Some(worker) = &self.document_worker {
            let mut worker = worker.lock().await;
            if worker.start().is_err() {
                let _ = worker.stop().await;
                return Err(KnowledgeError::Startup("document_worker"));
            }
        }
        if let Some(worker) = &self.migration_worker {
            let (stop, signal) = watch::channel(false);
            let worker = worker.clone();
            let handle = tokio::spawn(async move { worker.run(signal).await.map_err(|_| ()) });
            *self.migration_runner.lock().await = Some(MigrationRunner { stop, handle });
        }
        self.started.store(true, Ordering::Release);
        Ok(())
    }
    /// Source order: readiness false, drain, document worker, migration worker,
    /// then close Knowledge-owned PostgreSQL. No pool is closed by a constructor.
    pub async fn stop(&self) -> Result<()> {
        let _transition = self.transition.lock().await;
        self.started.store(false, Ordering::Release);
        self.pdf_ready.store(false, Ordering::Release);
        self.drain.begin_drain();
        let drained = self.drain.wait_drained(Duration::from_secs(30)).await;
        let mut failed = if drained { None } else { Some("request_drain") };
        if let Some(worker) = &self.document_worker {
            if worker.lock().await.stop().await.is_err() {
                failed = Some("document_worker");
            }
        }
        if let Some(runner) = self.migration_runner.lock().await.take() {
            let _ = runner.stop.send(true);
            if !matches!(runner.handle.await, Ok(Ok(()))) {
                failed = Some("migration_worker");
            }
        }
        self.pool.close().await;
        failed.map_or(Ok(()), |name| Err(KnowledgeError::Shutdown(name)))
    }
    pub async fn snapshot(&self) -> Value {
        let timeout = Duration::from_secs(1);
        let (database, qdrant) =
            tokio::join!(self.database_ready(timeout), self.qdrant_ready(timeout));
        let document = self
            .document_worker
            .as_ref()
            .is_some_and(|worker| worker.try_lock().is_ok_and(|worker| worker.is_running()));
        let migration = self
            .migration_runner
            .lock()
            .await
            .as_ref()
            .is_some_and(|runner| !runner.handle.is_finished());
        let status = |v| if v { "healthy" } else { "unavailable" };
        let mut core = json!({"startup":status(self.started.load(Ordering::Acquire)),"database":status(database),"qdrant":status(qdrant),"drain":status(!self.drain.draining())});
        if let Some(embeddings) = &self.local_semantic {
            let available = matches!(
                tokio::time::timeout(timeout, embeddings.probe_local_semantic(false)).await,
                Ok(Ok(()))
            );
            core["local_semantic_model"] = json!(status(available));
        }
        let pdf_ready = self.pdf_ready.load(Ordering::Acquire);
        if self.pdf_parser.is_some() {
            core["pdf_text"] = json!(status(pdf_ready));
        }
        let mut upload_formats = vec!["txt", "md", "html", "docx"];
        if pdf_ready {
            upload_formats.push("pdf");
        }
        let capabilities = json!({
            "upload_formats": upload_formats,
            "pdf_text": {
                "configured": self.pdf_parser.is_some(),
                "status": if self.pdf_parser.is_none() { "disabled" } else { status(pdf_ready) },
                "startup_probe_passed": pdf_ready,
            },
        });
        if self.role.api() {
            core["api"] = json!("healthy");
        }
        if self.role.worker() {
            core["worker"] = json!(status(document && migration));
        }
        let ready = core
            .as_object()
            .is_some_and(|items| items.values().all(|value| value == "healthy"));
        json!({"status":if ready {"ready"}else{"not_ready"},"core_ready":ready,"runtime_role":self.role.as_str(),"core":core,"capabilities":capabilities})
    }
}
