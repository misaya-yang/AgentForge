//! Durable fair dispatch. The local queue is an optimization; PostgreSQL owns
//! waiting generations, admission CAS and cross-replica document ownership.
use super::{IngestError, IngestionService, Result, object, store::KnowledgeWriteStore, string};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::{
    sync::{Mutex, mpsc, watch},
    task::JoinHandle,
};
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct IngestTask {
    pub dataset_id: String,
    pub document_id: String,
}
#[derive(Clone, Debug)]
pub struct WorkerConfig {
    pub concurrency: usize,
    pub queue_capacity: usize,
    pub dispatch_interval: Duration,
    pub recovery_interval: Duration,
    pub recovery_threshold_minutes: i32,
}
impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            concurrency: 2,
            queue_capacity: 100,
            dispatch_interval: Duration::from_secs(1),
            recovery_interval: Duration::from_secs(60),
            recovery_threshold_minutes: 30,
        }
    }
}
#[derive(Default)]
pub(crate) struct WorkerObserver {
    workers: Vec<tokio::task::AbortHandle>,
    stop: Option<watch::Receiver<bool>>,
    sender: Option<mpsc::Sender<IngestTask>>,
}
impl IngestionService {
    pub fn worker_status(&self) -> Result<Value> {
        let observer = self
            .worker_observer
            .read()
            .map_err(|_| IngestError::Database)?;
        Ok(
            json!({"running":observer.stop.as_ref().is_some_and(|stop|!*stop.borrow()),"queue_size":observer.sender.as_ref().map_or(0,|sender|sender.max_capacity()-sender.capacity()),"worker_count":observer.workers.len(),"workers_alive":observer.workers.iter().map(|handle|!handle.is_finished()).collect::<Vec<_>>()}),
        )
    }
}
pub struct KnowledgeWorker {
    service: Arc<IngestionService>,
    config: WorkerConfig,
    sender: mpsc::Sender<IngestTask>,
    receiver: Arc<Mutex<mpsc::Receiver<IngestTask>>>,
    in_queue: Arc<Mutex<HashSet<IngestTask>>>,
    stop: watch::Sender<bool>,
    handles: Vec<JoinHandle<()>>,
}
impl KnowledgeWorker {
    pub fn new(service: Arc<IngestionService>, config: WorkerConfig) -> Self {
        let (sender, receiver) = mpsc::channel(config.queue_capacity.max(1));
        let (stop, _) = watch::channel(false);
        Self {
            service,
            config,
            sender,
            receiver: Arc::new(Mutex::new(receiver)),
            in_queue: Arc::new(Mutex::new(HashSet::new())),
            stop,
            handles: vec![],
        }
    }
    pub fn is_running(&self) -> bool {
        self.config.concurrency > 0
            && !self.handles.is_empty()
            && !*self.stop.borrow()
            && self.handles.iter().all(|h| !h.is_finished())
    }
    pub fn start(&mut self) -> Result<()> {
        if !self.handles.is_empty() {
            return Ok(());
        }
        if self.service.store.pool.options().get_max_connections()
            < self.config.concurrency as u32 + 2
        {
            return Err(IngestError::Validation("ingestion workers require database pool capacity for document and publication leases".into()));
        }
        if self.service.indexer.vectors.is_read_only()
            || self.service.vector_job_actor_authority.is_none()
        {
            return Err(IngestError::Validation(
                "Knowledge worker requires write-role vectors and current actor authority".into(),
            ));
        }
        self.stop.send_replace(false);
        for _ in 0..self.config.concurrency.max(1) {
            let service = self.service.clone();
            let queue = self.receiver.clone();
            let in_queue = self.in_queue.clone();
            let mut stop = self.stop.subscribe();
            self.handles.push(tokio::spawn(async move {loop{let task=tokio::select!{biased;_ = stop.changed()=>break, task=async{queue.lock().await.recv().await}=>match task{Some(t)=>t,None=>break}};
                // Finish an owned generation before acknowledging stop, so an
                // abandoned CPU job cannot publish after another owner starts.
                let result=super::model_plane_worker::ingest(&service,&task.dataset_id,&task.document_id).await;
                let _=result;in_queue.lock().await.remove(&task);
            }}));
        }
        let service = self.service.clone();
        let sender = self.sender.clone();
        let in_queue = self.in_queue.clone();
        let interval = self.config.dispatch_interval;
        let mut stop = self.stop.subscribe();
        self.handles.push(tokio::spawn(async move {
            let mut cursor = String::new();
            loop {
                if *stop.borrow() {
                    break;
                }
                if let Ok(rows) = service.store.list_queued(100, &cursor).await {
                    for row in rows {
                        cursor = string(&row, "tenant_id").into();
                        let task = IngestTask {
                            dataset_id: string(&row, "dataset_id").into(),
                            document_id: string(&row, "document_id").into(),
                        };
                        let mut set = in_queue.lock().await;
                        if set.contains(&task) {
                            continue;
                        }
                        match sender.try_send(task.clone()) {
                            Ok(()) => {
                                set.insert(task);
                            }
                            Err(_) => break,
                        }
                    }
                }
                tokio::select! {_ = stop.changed()=>break,_ = tokio::time::sleep(interval)=>{}}
            }
        }));
        let service = self.service.clone();
        let mut stop = self.stop.subscribe();
        self.handles.push(tokio::spawn(async move {let batches=super::batches::DocumentBatchStore::new(service.store.pool.clone());let worker=uuid::Uuid::new_v4().to_string();loop{if *stop.borrow(){break}let busy=super::batches::dispatch_one(&service,&batches,&worker).await.unwrap_or(false);tokio::select!{_=stop.changed()=>break,_=tokio::time::sleep(if busy{Duration::from_millis(10)}else{Duration::from_secs(1)})=>{}}}}));
        let service = self.service.clone();
        let mut stop = self.stop.subscribe();
        self.handles.push(tokio::spawn(async move {
            let actors = service.vector_job_actor_authority.as_ref().expect("validated actor authority");
            loop {
                if *stop.borrow() { break; }
                let busy = super::vector_jobs::dispatch_one(&service, actors).await.unwrap_or(false);
                tokio::select! { _ = stop.changed() => break, _ = tokio::time::sleep(if busy { Duration::from_millis(10) } else { Duration::from_secs(1) }) => {} }
            }
        }));
        let service = self.service.clone();
        let config = self.config.clone();
        let mut stop = self.stop.subscribe();
        self.handles.push(tokio::spawn(async move {loop{tokio::select!{_ = stop.changed()=>break,_ = tokio::time::sleep(config.recovery_interval)=>{let _=recover_stuck(&service,config.recovery_threshold_minutes).await;}}}}));
        {
            let mut observer = self
                .service
                .worker_observer
                .write()
                .map_err(|_| IngestError::Database)?;
            observer.workers = self
                .handles
                .iter()
                .take(self.config.concurrency.max(1))
                .map(JoinHandle::abort_handle)
                .collect();
            observer.stop = Some(self.stop.subscribe());
            observer.sender = Some(self.sender.clone());
        }
        Ok(())
    }
    pub async fn enqueue(
        &self,
        dataset: &str,
        document: &str,
        action: Option<&str>,
    ) -> Result<bool> {
        if !self
            .service
            .store
            .enqueue(dataset, document, action, None, None, false)
            .await?
        {
            return Ok(false);
        }
        self.enqueue_claimed(dataset, document).await?;
        Ok(true)
    }
    pub async fn enqueue_claimed(&self, dataset: &str, document: &str) -> Result<()> {
        let task = IngestTask {
            dataset_id: dataset.into(),
            document_id: document.into(),
        };
        let mut set = self.in_queue.lock().await;
        if set.insert(task.clone()) {
            if self.sender.try_send(task.clone()).is_err() {
                set.remove(&task); /* Durable discovery will republish it. */
            }
        }
        Ok(())
    }
    pub async fn stop(&mut self) -> Result<()> {
        let _ = self.stop.send(true);
        for handle in self.handles.drain(..) {
            handle.await.map_err(|_| IngestError::Cancelled)?
        }
        self.service
            .worker_observer
            .write()
            .map_err(|_| IngestError::Database)?
            .workers
            .clear();
        Ok(())
    }
}
/// Retry only generations whose document lease is no longer owned. Specialized
/// publications retain their durable fence until their own recovery path exists.
pub async fn recover_stuck(service: &IngestionService, threshold_minutes: i32) -> Result<usize> {
    let negative:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(ds) FROM knowledge.datasets ds WHERE ds.is_deleted=FALSE AND ds.content_revision<0 AND EXISTS(SELECT 1 FROM knowledge.document_pipeline_executions e WHERE e.dataset_id=ds.dataset_id AND e.status='running' AND e.manifest?'special_publication') LIMIT 100").fetch_all(&service.store.pool).await?;
    for dataset in negative {
        let typed: super::Dataset = serde_json::from_value(dataset)?;
        let coordinator = super::special::SpecialPublicationCoordinator {
            service: service.clone(),
        };
        match coordinator.recover_unfinished(&typed).await {
            Ok(_) | Err(IngestError::LeaseBusy) => {}
            Err(_) => { /* Keep the negative fence: no guessed owner or cleanup. */ }
        }
    }
    let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(d) FROM knowledge.documents d JOIN knowledge.datasets ds USING(dataset_id) WHERE d.status IN ('parsing','splitting','indexing') AND d.updated_at<NOW()-($1::integer*INTERVAL '1 minute') AND ds.is_deleted=FALSE AND ds.is_archived=FALSE ORDER BY d.updated_at LIMIT 100")
        .bind(threshold_minutes.max(1)).fetch_all(&service.store.pool).await?;
    let mut recovered = 0;
    let mut requeued = 0;
    for row in rows {
        let dataset = string(&row, "dataset_id");
        let document = string(&row, "document_id");
        let metadata = object(&row["metadata"]);
        let mut lease = match service.store.document_lease(dataset, document).await {
            Ok(lease) => lease,
            Err(IngestError::LeaseBusy) => continue,
            Err(e) => return Err(e),
        };
        let current =
            KnowledgeWriteStore::document_on(lease.connection(), dataset, document).await?;
        if current["updated_at"] != row["updated_at"] || current["status"] != row["status"] {
            lease.release().await?;
            continue;
        }
        if metadata.get("_special_publication_generation_id").is_some() {
            let execution = string(&metadata, "_document_pipeline_execution_id");
            let dataset_row = KnowledgeWriteStore::dataset_on(lease.connection(), dataset).await?;
            let typed: super::Dataset = serde_json::from_value(dataset_row)?;
            let coordinator = super::special::SpecialPublicationCoordinator {
                service: service.clone(),
            };
            if coordinator
                .abort_preparing(&typed, execution, true)
                .await
                .is_ok()
            {
                recovered += 1;
            }
            lease.release().await?;
            continue;
        }
        // Keep the original immutable execution/rule pin. Recovery never falls
        // back to today's mutable chunking configuration.
        let execution = string(&metadata, "_document_pipeline_execution_id");
        let rule = string(&row, "process_rule_id");
        if execution.is_empty() && rule.is_empty() {
            lease.release().await?;
            continue;
        }
        if execution.is_empty() && !rule.is_empty() {
            let stage = string(&row, "status");
            sqlx::query("UPDATE knowledge.documents SET metadata=COALESCE(metadata,'{}'::jsonb)||jsonb_build_object('_document_ingest_action','recover','_document_recover_stage',$3::text) WHERE dataset_id=$1 AND document_id=$2").bind(dataset).bind(document).bind(if ["parsing","splitting","indexing"].contains(&stage){stage}else{"parsing"}).execute(lease.connection()).await?;
        }
        if KnowledgeWriteStore::requeue_cancelled(lease.connection(), dataset, document).await? {
            recovered += 1;
            requeued += 1;
        }
        lease.release().await?;
    }
    crate::migration_knowledge_query::pipeline_metrics::recovered(recovered as u64, requeued);
    Ok(recovered)
}
