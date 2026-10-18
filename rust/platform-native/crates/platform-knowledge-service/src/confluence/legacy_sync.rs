//! Dormant legacy sync execution. Uses source generation SQL and legacy source
//! ownership, without minting a manual task actor or activating polling routes.
use super::*;
use super::{legacy_management::LegacyManagement, sync::Execution};
use chrono::{DateTime, Utc};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Deref,
    time::Duration,
};
#[derive(Clone)]
pub struct LegacySyncService {
    pub sources: Arc<Sources>,
    pub management: LegacyManagement,
    pub worker: Option<Arc<ingest::worker::KnowledgeWorker>>,
    tasks: Arc<tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    pub stale_sync_timeout_seconds: i64,
}
impl Deref for LegacySyncService {
    type Target = Sources;
    fn deref(&self) -> &Sources {
        &self.sources
    }
}
#[derive(Clone, Default)]
pub struct SyncResult {
    pub task_id: Option<String>,
    pub total_pages: usize,
    pub synced_pages: usize,
    pub skipped_pages: usize,
    pub failed_pages: usize,
    pub created_documents: Vec<String>,
    pub updated_documents: Vec<String>,
    pub deleted_documents: Vec<String>,
    pub errors: Vec<Value>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}
impl SyncResult {
    pub fn new(task: Option<String>) -> Self {
        Self {
            task_id: task,
            started_at: Some(Utc::now()),
            ..Default::default()
        }
    }
    pub fn value(&self) -> Value {
        let stamp = |t: Option<DateTime<Utc>>| {
            t.map(|t| t.naive_utc().format("%Y-%m-%dT%H:%M:%S%.f").to_string())
        };
        json!({"task_id":self.task_id,"total_pages":self.total_pages,"synced_pages":self.synced_pages,"skipped_pages":self.skipped_pages,"failed_pages":self.failed_pages,"created_documents":self.created_documents,"updated_documents":self.updated_documents,"deleted_documents":self.deleted_documents,"errors":self.errors,"success_rate":if self.total_pages==0{0.}else{(self.synced_pages+self.skipped_pages)as f64/self.total_pages as f64},"has_errors":self.failed_pages>0||!self.errors.is_empty(),"started_at":stamp(self.started_at),"completed_at":stamp(self.completed_at)})
    }
}
fn timestamp(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|t| t.with_timezone(&Utc))
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|t| t.and_utc())
        })
}
impl LegacySyncService {
    pub fn new(
        sources: Arc<Sources>,
        worker: Option<Arc<ingest::worker::KnowledgeWorker>>,
    ) -> Self {
        Self {
            management: LegacyManagement::new(sources.clone()),
            sources,
            worker,
            tasks: Arc::new(tokio::sync::Mutex::new(vec![])),
            stale_sync_timeout_seconds: 3600,
        }
    }
    pub async fn close(&self) {
        let handles = std::mem::take(&mut *self.tasks.lock().await);
        for handle in handles {
            handle.abort();
            let _ = handle.await;
        }
        self.management.clients.close().await;
    }
    pub async fn writable(&self, id: &str) -> Result<Value> {
        let ds: Value =
            sqlx::query_scalar("SELECT to_jsonb(d) FROM datasets d WHERE dataset_id=$1")
                .bind(id)
                .fetch_optional(ai_platform_knowledge_domain::database::executor(&self.pool))
                .await?
                .ok_or_else(|| Error::missing("Dataset not found"))?;
        writable_dataset(&ds)?;
        Ok(ds)
    }
    pub(super) async fn guard(&self, e: &Execution) -> Result<query::types::UserContext> {
        self.writable(text(&e.binding, "dataset_id")).await?;
        Ok(e.actor.clone())
    }
    pub(super) async fn guard_on(&self, e: &Execution, conn: &mut PgConnection) -> Result<()> {
        self.guard_deletion_on(e, conn, None).await
    }
    pub(super) async fn guard_deletion(
        &self,
        e: &Execution,
        target: Option<&str>,
    ) -> Result<query::types::UserContext> {
        let mut conn = self.pool.acquire().await?;
        self.guard_deletion_on(e, &mut conn, target).await?;
        Ok(e.actor.clone())
    }
    pub(super) async fn guard_deletion_on(
        &self,
        e: &Execution,
        conn: &mut PgConnection,
        target: Option<&str>,
    ) -> Result<()> {
        if let Some(target) = target {
            let ds = locked_dataset(conn, &e.actor, text(&e.binding, "dataset_id"), false).await?;
            return deletion_writable(&ds, target);
        }
        let ds: Value =
            sqlx::query_scalar("SELECT to_jsonb(d) FROM datasets d WHERE dataset_id=$1 FOR UPDATE")
                .bind(text(&e.binding, "dataset_id"))
                .fetch_optional(ai_platform_knowledge_domain::database::connection(
                    &mut *conn,
                ))
                .await?
                .ok_or_else(|| Error::missing("Dataset not found"))?;
        if let Some(target) = target {
            deletion_writable(&ds, target)
        } else {
            writable_dataset(&ds)
        }
    }
    async fn execution(&self, binding: Value, task: &str) -> Result<Execution> {
        let connection = self
            .sources
            .connection(text(&binding, "connection_id"))
            .await?
            .ok_or_else(|| Error::missing("Connection not found"))?;
        let actor = query::types::UserContext {
            user_id: text(&binding, "created_by").into(),
            tenant_id: text(&binding, "tenant_id").into(),
            user_tier: "normal".into(),
            user_type: "user".into(),
            roles: vec!["user".into()],
            is_authenticated: true,
        };
        Ok(Execution {
            binding,
            connection,
            actor,
            task_id: task.into(),
        })
    }
    async fn binding(&self, id: &str) -> Result<Value> {
        self.management
            .binding(id)
            .await?
            .ok_or_else(|| Error::missing("Binding not found"))
    }
    async fn pages(&self, id: &str) -> Result<Vec<Value>> {
        Ok(sqlx::query_scalar(
            "SELECT to_jsonb(p) FROM confluence_pages p WHERE binding_id=$1 ORDER BY title ASC",
        )
        .bind(id)
        .fetch_all(ai_platform_knowledge_domain::database::executor(&self.pool))
        .await?)
    }
    async fn recover_stale(&self, binding: &Value) -> Result<bool> {
        let mut tasks = Vec::<Value>::new();
        for status in ["pending", "processing"] {
            let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(t) FROM confluence_sync_tasks t WHERE binding_id=$1 AND status=$2 ORDER BY priority DESC,created_at ASC LIMIT 100").bind(text(binding,"binding_id")).bind(status).fetch_all(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
            tasks.extend(rows);
        }
        let cutoff =
            Utc::now() - chrono::Duration::seconds(self.stale_sync_timeout_seconds.max(60));
        for task in &tasks {
            let touched = ["updated_at", "started_at", "created_at"]
                .iter()
                .find_map(|k| task[*k].as_str().filter(|s| !s.is_empty()));
            if touched.and_then(timestamp).is_none_or(|t| t > cutoff) {
                return Ok(false);
            }
        }
        for task in tasks {
            sqlx::query("UPDATE confluence_sync_tasks SET status='failed',error='Recovered stale Confluence sync after restart',completed_at=NOW(),updated_at=NOW() WHERE task_id=$1").bind(text(&task,"task_id")).execute(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
        }
        self.management.persist_binding(text(binding,"binding_id"),&json!({"status":"pending","last_error":"Recovered stale Confluence sync after restart"})).await?;
        Ok(true)
    }
    async fn start(
        &self,
        id: &str,
        kind: &str,
        priority: i32,
        force: bool,
        pages: Option<Vec<String>>,
    ) -> Result<String> {
        let binding = self.binding(id).await?;
        self.writable(text(&binding, "dataset_id")).await?;
        if binding["status"] == "syncing" && !force && !self.recover_stale(&binding).await? {
            return Err(Error::conflict("A sync is already in progress"));
        }
        let task = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO confluence_sync_tasks(task_id,binding_id,task_type,priority,owner_id,status,retry_count,max_retries,progress,total_items,processed_items) VALUES($1,$2,$3,$4,$5,'pending',0,3,0,0,0)").bind(&task).bind(id).bind(kind).bind(priority).bind(owner(&binding)).execute(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
        self.management
            .persist_binding(id, &json!({"status":"syncing"}))
            .await?;
        let this = self.clone();
        let id = id.to_owned();
        let task_id = task.clone();
        let incremental = kind == "incremental_sync";
        let handle = tokio::spawn(async move {
            if this.run(&id, &task_id, incremental, pages).await.is_err() {
                eprintln!("Legacy Confluence background persistence failed");
            }
        });
        let mut tasks = self.tasks.lock().await;
        tasks.retain(|h| !h.is_finished());
        tasks.push(handle);
        Ok(task)
    }
    pub async fn trigger_sync(
        &self,
        binding: &str,
        force: bool,
        pages: Option<Vec<String>>,
    ) -> Result<String> {
        let kind = if pages.as_ref().is_some_and(|v| !v.is_empty()) {
            "partial_sync"
        } else {
            "full_sync"
        };
        self.start(binding, kind, 1, force, pages.filter(|v| !v.is_empty()))
            .await
    }
    pub async fn incremental_sync(&self, binding: &str, force: bool) -> Result<String> {
        self.start(binding, "incremental_sync", 1, force, None)
            .await
    }
    pub async fn import_space(&self, binding: &str, force: bool) -> Result<Value> {
        match self.start(binding, "full_sync", 0, force, None).await {
            Ok(task) => Ok(SyncResult::new(Some(task)).value()),
            Err(error) if error.detail == "A sync is already in progress" => {
                let mut result = SyncResult::new(None);
                result.completed_at = Some(Utc::now());
                result
                    .errors
                    .push(json!({"error":"Sync already in progress"}));
                Ok(result.value())
            }
            Err(error) => Err(error),
        }
    }
    async fn descendants(
        &self,
        credentials: &credentials::Credentials,
        root: &str,
        max: i64,
    ) -> Result<Vec<String>> {
        let mut result = Vec::new();
        let mut stack = vec![(root.to_owned(), 0i64)];
        while let Some((id, depth)) = stack.pop() {
            result.push(id.clone());
            if depth < max {
                let children =
                    self.sources.children(credentials, &id).await.map_err(|_| {
                        Error::unavailable("Confluence descendant discovery failed")
                    })?;
                for child in children.into_iter().rev() {
                    stack.push((child, depth + 1));
                }
            }
        }
        Ok(result)
    }
    async fn modified(
        &self,
        credentials: &credentials::Credentials,
        key: &str,
        since: &str,
    ) -> Result<Vec<String>> {
        let key = discovery::escape_cql_value(key)?;
        let since = timestamp(since).ok_or_else(|| {
            Error::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid last sync timestamp",
            )
        })?;
        let cql = format!(
            "space={key} AND type=page AND lastModified > \"{}\"",
            since.format("%Y-%m-%d %H:%M")
        );
        let mut start = 0usize;
        let mut ids = Vec::new();
        loop {
            let data = self
                .v1(
                    credentials,
                    &["content", "search"],
                    &[
                        ("cql", cql.clone()),
                        ("limit", "100".into()),
                        ("start", start.to_string()),
                        ("expand", "version".into()),
                    ],
                )
                .await?;
            let rows = data["results"].as_array().cloned().unwrap_or_default();
            if rows.is_empty() {
                break;
            }
            for row in &rows {
                ids.push(client::scalar(&row["id"]));
            }
            start += rows.len();
            if start >= data["totalSize"].as_u64().unwrap_or(0) as usize || rows.len() < 100 {
                break;
            }
        }
        Ok(ids)
    }
    async fn v1(
        &self,
        c: &credentials::Credentials,
        path: &[&str],
        params: &[(&str, String)],
    ) -> Result<Value> {
        if !client::cloud_domain(&c.domain) {
            return Err(Error::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Unsupported Confluence hostname",
            ));
        }
        let mut url = reqwest::Url::parse(&format!("https://{}/wiki/rest/api/", c.domain))
            .map_err(|_| Error::unavailable("Confluence origin unavailable"))?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| Error::unavailable("Confluence origin unavailable"))?;
            segments.pop_if_empty();
            for part in path {
                segments.push(part);
            }
        }
        for attempt in 0..self.max_retries {
            let response = self
                .client
                .get(url.clone())
                .query(params)
                .basic_auth(&c.email, Some(&c.token))
                .send()
                .await;
            match response {
                Ok(response) if response.status() == StatusCode::TOO_MANY_REQUESTS => {
                    let delay = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(5);
                    tokio::time::sleep(Duration::from_secs(delay)).await;
                }
                Ok(response)
                    if response.status().is_server_error() && attempt + 1 < self.max_retries =>
                {
                    tokio::time::sleep(Duration::from_secs(1 << attempt)).await
                }
                Ok(mut response) if response.status().is_success() => {
                    let mut bytes = Vec::new();
                    while let Some(chunk) = response
                        .chunk()
                        .await
                        .map_err(|_| Error::unavailable("Confluence provider request failed"))?
                    {
                        if bytes.len() + chunk.len() > 32 * 1024 * 1024 {
                            return Err(Error::unavailable(
                                "Confluence provider response too large",
                            ));
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                    return serde_json::from_slice(&bytes)
                        .map_err(|_| Error::unavailable("Invalid Confluence provider response"));
                }
                Err(_) if attempt + 1 < self.max_retries => {
                    tokio::time::sleep(Duration::from_secs(1 << attempt)).await
                }
                _ => return Err(Error::unavailable("Confluence provider request failed")),
            }
        }
        Err(Error::unavailable("Confluence provider request failed"))
    }
    async fn run(
        &self,
        id: &str,
        task: &str,
        mut incremental: bool,
        specific: Option<Vec<String>>,
    ) -> Result<Value> {
        let mut result = SyncResult::new(Some(task.into()));
        let binding = self.binding(id).await?;
        let e = self.execution(binding, task).await?;
        self.guard(&e).await?;
        let partial = specific.is_some();
        let outcome=async{
            let c=self.management.credentials(text(&e.connection,"connection_id")).await?;let existing=self.pages(id).await?;let prior=existing.iter().map(|p|(text(p,"page_id").to_owned(),p)).collect::<BTreeMap<_,_>>();
            if text(&e.binding,"last_sync_at").is_empty(){incremental=false;}
            let roots=e.binding["root_page_ids"].as_array().filter(|v|!v.is_empty()).map(|v|v.iter().map(client::scalar).collect::<Vec<_>>()).unwrap_or_else(||if text(&e.binding,"root_page_id").is_empty(){vec![]}else{vec![text(&e.binding,"root_page_id").into()]});
            let partial=specific.is_some();let pages=if let Some(pages)=specific{pages}else if incremental{self.modified(&c,text(&e.binding,"space_key"),text(&e.binding,"last_sync_at")).await?}else if roots.is_empty(){prior.keys().cloned().collect()}else{let mut seen=BTreeSet::new();let mut all=Vec::new();for root in &roots{for page in self.descendants(&c,root,e.binding["max_depth"].as_i64().unwrap_or(10)).await?{if seen.insert(page.clone()){all.push(page);}}}all};
            result.total_pages=pages.len();sqlx::query("UPDATE confluence_sync_tasks SET status='processing',total_items=$2,started_at=NOW(),updated_at=NOW() WHERE task_id=$1").bind(task).bind(pages.len()as i32).execute(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
            let seen=pages.iter().cloned().collect::<BTreeSet<_>>();
            for (position,page) in pages.iter().enumerate(){
                if prior.get(page).is_some_and(|p|text(p,"document_id").is_empty())&&!partial{result.synced_pages+=1;continue;}
                match self.publish_page(&e,&c,page).await{Ok((document,changed,new))=>{if !changed&&!partial{result.skipped_pages+=1;continue;}result.synced_pages+=1;if new{result.created_documents.push(document.clone());}else if changed{result.updated_documents.push(document.clone());}self.enqueue(text(&e.binding,"dataset_id"),&document).await?;sqlx::query("UPDATE confluence_sync_tasks SET processed_items=$2,progress=$3,updated_at=NOW() WHERE task_id=$1").bind(task).bind((position+1)as i32).bind(((position+1)as f64/pages.len()as f64)*100.).execute(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;},Err(error)=>{result.failed_pages+=1;result.errors.push(json!({"page_id":page,"error":error.detail}));}}
            }
            if !partial && (incremental||!roots.is_empty()){
                for (page,record) in &prior{if seen.contains(page){continue;}let removed=if incremental{matches!(self.sources.page(&c,page).await,Err(error) if error.detail=="Confluence provider resource not found")}else{true};if removed{match self.detach_source(&e,record).await{Ok(true)=>result.deleted_documents.push(text(record,"document_id").into()),Ok(false)=>{},Err(error)=>result.errors.push(json!({"page_id":page,"error":error.detail}))}}}
            }
            if !partial&&(result.failed_pages>0||!result.errors.is_empty()){return Err(Error::unavailable("Confluence sync did not complete all pages"));}Ok::<_,Error>(())
        }.await;
        result.completed_at = Some(Utc::now());
        match outcome {
            Ok(()) => {
                let mut update =
                    json!({"status":"completed","last_sync_at":Utc::now(),"last_error":null});
                if !partial && !incremental {
                    update["synced_page_count"] = json!(result.synced_pages);
                    update["total_page_count"] = json!(result.total_pages);
                }
                self.management.persist_binding(id, &update).await?;
                sqlx::query("UPDATE confluence_sync_tasks SET status='completed',progress=100,completed_at=NOW(),result=$2,updated_at=NOW() WHERE task_id=$1").bind(task).bind(result.value()).execute(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
            }
            Err(error) => {
                result.errors.push(json!({"error":error.detail}));
                self.management
                    .persist_binding(id, &json!({"status":"error","last_error":error.detail}))
                    .await?;
                sqlx::query("UPDATE confluence_sync_tasks SET status='failed',completed_at=NOW(),result=$2,error=$3,updated_at=NOW() WHERE task_id=$1").bind(task).bind(result.value()).bind(error.detail.as_str().unwrap_or("Confluence sync failed")).execute(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
            }
        }
        Ok(result.value())
    }
    pub(super) async fn enqueue(&self, dataset: &str, document: &str) -> Result<()> {
        let worker = self
            .worker
            .as_ref()
            .ok_or_else(|| Error::unavailable("knowledge worker is unavailable"))?;
        worker
            .enqueue_claimed(dataset, document)
            .await
            .map_err(|_| Error::unavailable("durable claimed-queue publication failed"))
    }
    pub async fn sync_page_record(
        &self,
        id: &str,
        user: Option<&query::types::UserContext>,
    ) -> Result<Value> {
        let record = if let Some(user) = user {
            Some(self.management.get_page(id, user).await?)
        } else {
            sqlx::query_scalar::<_, Value>("SELECT to_jsonb(p) FROM confluence_pages p WHERE id=$1")
                .bind(id)
                .fetch_optional(ai_platform_knowledge_domain::database::executor(&self.pool))
                .await?
        };
        let Some(record) = record else {
            return Ok(json!({"status":"error","message":"Page record not found"}));
        };
        let Some(binding) = self.management.binding(text(&record, "binding_id")).await? else {
            return Ok(json!({"status":"error","message":"Binding not found"}));
        };
        self.writable(text(&binding, "dataset_id")).await?;
        let e = self.execution(binding, "").await?;
        let result = async {
            let c = self.management.credentials(text(&e.connection,"connection_id")).await?;
            let (document,_,new) = self.publish_page(&e,&c,text(&record,"page_id")).await?;
            self.enqueue(text(&e.binding,"dataset_id"),&document).await?;
            let mut response = json!({"status":"success","action":if new{"created"}else{"updated"},"document_id":document});
            if !new {
                response["image_count"] = json!(self.document(&document).await?["metadata"]["image_count"].as_u64().unwrap_or(0));
            }
            Ok::<_,Error>(response)
        }.await;
        Ok(match result {
            Ok(v) => v,
            Err(error) => json!({"status":"error","message":error.detail}),
        })
    }
}
impl scheduler::SyncOperations for LegacySyncService {
    fn incremental_sync<'a>(
        &'a self,
        binding: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            Ok(json!(
                LegacySyncService::incremental_sync(self, binding, false).await?
            ))
        })
    }
    fn trigger_sync<'a>(
        &'a self,
        binding: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            Ok(json!(
                LegacySyncService::trigger_sync(self, binding, false, None).await?
            ))
        })
    }
    fn sync_page<'a>(
        &'a self,
        page: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move { self.sync_page_record(page, None).await })
    }
}
use std::{future::Future, pin::Pin};
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn result_rate_counts_skips_and_failure_independently() {
        let mut r = SyncResult::new(None);
        assert_eq!(r.value()["success_rate"], 0.);
        r.total_pages = 4;
        r.synced_pages = 1;
        r.skipped_pages = 2;
        r.failed_pages = 1;
        assert_eq!(r.value()["success_rate"], 0.75);
        assert_eq!(r.value()["has_errors"], true);
    }
}

pub struct UrlImport<'a> {
    pub url: &'a str,
    pub dataset_id: &'a str,
    pub connection_id: Option<&'a str>,
    pub tenant_id: Option<&'a str>,
    pub metadata: Option<&'a Value>,
    pub created_by: Option<&'a str>,
    pub include_children: bool,
    pub max_depth: i64,
    pub user: Option<&'a query::types::UserContext>,
}
const SOURCE_METADATA: &[&str] = &[
    "_document_lifecycle_reindex",
    "_document_upload_generation",
    "_document_upload_failed",
    "_confluence_sync_generation",
    "_document_ingest_action",
    "_document_recover_stage",
    "_document_pipeline_execution_id",
    "_document_pending_restore_version",
    "_document_restored_source_version",
    "_special_publication_generation_id",
    "_special_source_manifest",
    "_confluence_image_source_generation",
    "_confluence_attachment_manifest",
    "skipped_confluence_attachments",
    "original_file_key",
    "original_filename",
    "original_mime_type",
    "processing_mode",
    "extracted_images",
    "image_count",
    "images_embedded",
    "embedded_image_count",
    "ocr_processed",
    "structured_parsing",
];
impl LegacySyncService {
    pub(super) async fn document(&self, id: &str) -> Result<Value> {
        sqlx::query_scalar("SELECT to_jsonb(d) FROM documents d WHERE document_id=$1")
            .bind(id)
            .fetch_optional(ai_platform_knowledge_domain::database::executor(&self.pool))
            .await?
            .ok_or_else(|| Error::missing("Confluence document disappeared after source publish"))
    }
    pub async fn import_from_url(&self, request: UrlImport<'_>) -> Result<Value> {
        if request
            .metadata
            .and_then(Value::as_object)
            .is_some_and(|m| m.keys().any(|k| SOURCE_METADATA.contains(&k.as_str())))
        {
            return Err(Error::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Confluence document metadata contains a reserved key",
            ));
        }
        let domain = discovery::parse_domain_from_url(request.url);
        let connection=if let Some(id)=request.connection_id.filter(|s|!s.is_empty()){self.sources.connection(id).await?}else{sqlx::query_scalar("SELECT to_jsonb(c) FROM confluence_connections c WHERE domain=$1 AND ($2::text IS NULL OR tenant_id=$2) AND status='active' ORDER BY created_at DESC LIMIT 1").bind(&domain).bind(request.tenant_id.filter(|s|!s.is_empty())).fetch_optional(ai_platform_knowledge_domain::database::executor(&self.pool)).await?}.ok_or_else(||Error::missing("No matching Confluence connection found"))?;
        legacy_management::verify_import_access(
            &connection,
            request.tenant_id,
            request.created_by,
            request.user,
        )?;
        let dataset = self.writable(request.dataset_id).await?;
        let tenant = text(&dataset, "tenant_id");
        if tenant.is_empty() {
            return Err(Error::unavailable(
                "Confluence dataset tenant_id is required",
            ));
        }
        // The source import call takes dataset tenant authority, not the optional
        // connection lookup tenant, into document creation.
        let binding = json!({"binding_id":null,"connection_id":connection["connection_id"],"dataset_id":request.dataset_id,"tenant_id":tenant,"created_by":request.created_by,"sync_images":true,"extra_metadata":request.metadata});
        let actor = query::types::UserContext {
            user_id: request.created_by.unwrap_or("").into(),
            tenant_id: tenant.into(),
            user_tier: "normal".into(),
            user_type: "user".into(),
            roles: vec!["user".into()],
            is_authenticated: true,
        };
        let mut e = Execution {
            binding,
            connection,
            actor,
            task_id: String::new(),
        };
        let c = self
            .management
            .credentials(text(&e.connection, "connection_id"))
            .await?;
        let page_id = discovery::parse_page_id_from_url(request.url)?;
        let page = self.sources.page(&c, &page_id).await?;
        let space_key = if let Some(space) = page["spaceId"].as_str() {
            self.sources
                .get_space(&c, space)
                .await
                .map(|s| s.space_key)
                .unwrap_or_default()
        } else {
            discovery::parse_space_key_from_url(request.url).unwrap_or_default()
        };
        e.binding["space_key"] = json!(space_key);
        let (id, _, _) = self.publish_page(&e, &c, &page_id).await?;
        self.enqueue(request.dataset_id, &id).await?;
        let document = self.document(&id).await?;
        let mut result = json!({"status":"success","document_id":id,"title":document["title"],"page_info":{"page_id":page_id,"title":page["title"],"version":page["version"]["number"],"space_key":space_key}});
        if request.include_children && request.max_depth > 0 {
            let mut documents = Vec::new();
            e.binding["extra_metadata"] = Value::Null;
            for child in self
                .sources
                .children(&c, &page_id)
                .await?
                .into_iter()
                .take(20)
            {
                let child_result = async {
                    let (id, _, _) = self.publish_page(&e, &c, &child).await?;
                    self.enqueue(request.dataset_id, &id).await?;
                    self.document(&id).await
                }
                .await;
                if let Ok(document) = child_result {
                    documents.push(
                        json!({"document_id":document["document_id"],"title":document["title"]}),
                    );
                }
            }
            result["children_count"] = json!(documents.len());
            result["children"] = json!(documents);
        }
        Ok(result)
    }
    pub async fn sync_page_by_id(
        &self,
        binding: &str,
        page: &str,
        event: &str,
    ) -> Result<Option<String>> {
        let Some(binding) = self.management.binding(binding).await? else {
            return Ok(None);
        };
        self.writable(text(&binding, "dataset_id")).await?;
        let e = self.execution(binding, "").await?;
        if ["removed", "trashed"].contains(&event) {
            let record: Option<Value> = sqlx::query_scalar(
                "SELECT to_jsonb(p) FROM confluence_pages p WHERE binding_id=$1 AND page_id=$2",
            )
            .bind(text(&e.binding, "binding_id"))
            .bind(page)
            .fetch_optional(ai_platform_knowledge_domain::database::executor(&self.pool))
            .await?;
            let Some(record) = record else {
                return Ok(None);
            };
            if self.detach_source(&e, &record).await.is_err() {
                return Ok(None);
            }
            return Ok(record["document_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned));
        }
        let c = self
            .management
            .credentials(text(&e.connection, "connection_id"))
            .await?;
        let (id, _, _) = self.publish_page(&e, &c, page).await?;
        self.enqueue(text(&e.binding, "dataset_id"), &id).await?;
        Ok(Some(id))
    }
    pub async fn remove_pages(
        &self,
        ids: &[String],
        user: &query::types::UserContext,
        delete_documents: bool,
    ) -> Result<Value> {
        let mut removed = 0;
        let mut deleted = 0;
        let mut errors = Vec::new();
        for id in ids {
            let result = async {
                let page = self.management.get_page(id, user).await?;
                let binding = self
                    .management
                    .get_binding(text(&page, "binding_id"), user)
                    .await?;
                if delete_documents
                    && !text(&page, "document_id").is_empty()
                    && !text(&binding, "dataset_id").is_empty()
                {
                    let mut e = self.execution(binding, "").await?;
                    e.actor = user.clone();
                    let newly = self.detach_source(&e, &page).await?;
                    return Ok::<_, Error>((true, newly));
                }
                let changed = sqlx::query("DELETE FROM confluence_pages WHERE id=$1")
                    .bind(id)
                    .execute(ai_platform_knowledge_domain::database::executor(&self.pool))
                    .await?
                    .rows_affected()
                    > 0;
                Ok((changed, false))
            }
            .await;
            match result {
                Ok((true, newly)) => {
                    removed += 1;
                    if newly {
                        deleted += 1;
                    }
                }
                Ok((false, _)) => errors.push(json!({"id":id,"error":"Failed to delete"})),
                Err(error) => errors.push(json!({"id":id,"error":error.detail})),
            }
        }
        Ok(json!({"removed":removed,"documents_deleted":deleted,"errors":errors}))
    }
}

pub struct BindingCreate<'a> {
    pub connection_id: &'a str,
    pub tenant_id: &'a str,
    pub dataset_id: &'a str,
    pub space_key: &'a str,
    pub root_page_ids: Vec<String>,
    pub include_patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
    pub max_depth: i32,
    pub include_attachments: bool,
    pub include_comments: bool,
    pub sync_images: bool,
    pub image_max_size_bytes: i64,
    pub created_by: Option<&'a str>,
}
impl LegacySyncService {
    pub async fn create_space_binding(&self, request: BindingCreate<'_>) -> Result<Value> {
        let connection = self
            .sources
            .connection(request.connection_id)
            .await?
            .ok_or_else(|| Error::missing("Connection not found"))?;
        if !text(&connection, "tenant_id").is_empty()
            && text(&connection, "tenant_id") != request.tenant_id
        {
            return Err(Error::new(
                StatusCode::FORBIDDEN,
                "Access denied: connection belongs to different tenant",
            ));
        }
        let existing:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(b) FROM confluence_space_bindings b WHERE connection_id=$1 AND dataset_id=$2 ORDER BY created_at DESC LIMIT 100").bind(request.connection_id).bind(request.dataset_id).fetch_all(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
        if existing.iter().any(|b| b["space_key"] == request.space_key) {
            return Err(Error::conflict(
                "Binding already exists for this space and dataset",
            ));
        }
        let credentials = self.management.credentials(request.connection_id).await?;
        let space = self
            .sources
            .get_space_by_key(&credentials, request.space_key)
            .await?;
        let mut titles = Vec::new();
        for id in &request.root_page_ids {
            let title = self
                .sources
                .page(&credentials, id)
                .await
                .ok()
                .and_then(|p| p["title"].as_str().map(str::to_owned))
                .unwrap_or_else(|| format!("Page {id}"));
            titles.push(title);
        }
        let id = uuid::Uuid::new_v4().to_string();
        // The original save_confluence_binding persists only the compatibility
        // first root; multi-root/image options are absent from its INSERT list.
        let mut row:Value=sqlx::query_scalar("INSERT INTO confluence_space_bindings(binding_id,connection_id,tenant_id,dataset_id,space_key,space_id,space_name,root_page_id,root_page_title,include_patterns,exclude_patterns,max_depth,include_attachments,include_comments,status,last_sync_at,synced_page_count,total_page_count,last_error,created_by,owner_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,'pending',NULL,0,0,NULL,$15,$15) RETURNING to_jsonb(confluence_space_bindings)").bind(&id).bind(request.connection_id).bind(request.tenant_id).bind(request.dataset_id).bind(request.space_key).bind(space.space_id).bind(space.name).bind(request.root_page_ids.first()).bind(titles.first()).bind(json!(request.include_patterns)).bind(json!(request.exclude_patterns)).bind(request.max_depth).bind(request.include_attachments).bind(request.include_comments).bind(request.created_by).fetch_one(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
        if let Ok(task) = self.trigger_sync(&id, false, None).await {
            row["initial_sync_task_id"] = json!(task);
        }
        Ok(row)
    }
    pub async fn delete_binding(&self, id: &str, delete_documents: bool) -> Result<bool> {
        let Some(binding) = self.management.binding(id).await? else {
            return Ok(false);
        };
        if delete_documents {
            let e = self.execution(binding, "").await?;
            let pages:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(p) FROM confluence_pages p WHERE binding_id=$1 ORDER BY title ASC LIMIT 100").bind(id).fetch_all(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
            for page in pages {
                if !text(&page, "document_id").is_empty()
                    && self.detach_source(&e, &page).await.is_err()
                {
                    return Ok(false);
                }
            }
        }
        Ok(
            sqlx::query("DELETE FROM confluence_space_bindings WHERE binding_id=$1")
                .bind(id)
                .execute(ai_platform_knowledge_domain::database::executor(&self.pool))
                .await?
                .rows_affected()
                > 0,
        )
    }
    pub async fn batch_sync_pages(
        &self,
        ids: &[String],
        force: bool,
        user: Option<&query::types::UserContext>,
    ) -> Result<Value> {
        if ids.is_empty() {
            return Ok(json!({"status":"completed","total":0,"message":"No pages to sync"}));
        }
        let mut grouped = BTreeMap::<String, Vec<String>>::new();
        let mut order = Vec::new();
        let mut not_found = Vec::new();
        let mut denied = BTreeSet::new();
        for id in ids {
            let page: Option<Value> =
                sqlx::query_scalar("SELECT to_jsonb(p) FROM confluence_pages p WHERE id=$1")
                    .bind(id)
                    .fetch_optional(ai_platform_knowledge_domain::database::executor(&self.pool))
                    .await?;
            let Some(page) = page else {
                not_found.push(id.clone());
                continue;
            };
            let binding = text(&page, "binding_id");
            if let Some(user) = user {
                if !denied.contains(binding) {
                    match self.management.get_binding(binding, user).await {
                        Ok(_) => {}
                        Err(error) if error.status == StatusCode::FORBIDDEN => {
                            denied.insert(binding.to_owned());
                            continue;
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
            if denied.contains(binding) {
                continue;
            }
            if !grouped.contains_key(binding) {
                order.push(binding.to_owned());
            }
            grouped
                .entry(binding.into())
                .or_default()
                .push(text(&page, "page_id").into());
        }
        if grouped.is_empty() {
            return Ok(
                json!({"status":"error","message":"No valid pages found","not_found":not_found}),
            );
        }
        let mut results = Vec::new();
        let mut total = 0;
        let mut triggered = 0;
        for binding in order {
            let pages = grouped.remove(&binding).unwrap();
            let count = pages.len();
            total += count;
            match self.trigger_sync(&binding,force,Some(pages)).await{Ok(task)=>{triggered+=1;results.push(json!({"binding_id":binding,"task_id":task,"page_count":count,"status":"triggered"}));},Err(error)=>results.push(json!({"binding_id":binding,"error":error.detail,"page_count":count,"status":"failed"}))}
        }
        Ok(
            json!({"status":if triggered>0{"triggered"}else{"failed"},"total":total,"bindings":results,"not_found":if not_found.is_empty(){Value::Null}else{json!(not_found)}}),
        )
    }
}
