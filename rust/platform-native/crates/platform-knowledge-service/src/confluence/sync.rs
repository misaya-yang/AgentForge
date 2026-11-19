//! Process-owned manual full sync. Durable pending tasks are never stolen/replayed.
use super::*;
use super::{bindings::connection_scope, client::scalar};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, RwLock};
const BINDING_FIELDS: &[&str] = &[
    "binding_id",
    "tenant_id",
    "dataset_id",
    "connection_id",
    "owner_id",
    "created_by",
    "root_page_id",
    "root_page_ids",
    "max_depth",
    "include_patterns",
    "exclude_patterns",
    "include_attachments",
    "include_comments",
    "sync_images",
    "image_max_size_bytes",
];
#[derive(Clone)]
pub struct Execution {
    pub binding: Value,
    pub connection: Value,
    pub actor: query::types::UserContext,
    pub task_id: String,
}
pub struct Task {
    execution: Execution,
    handle: tokio::task::JoinHandle<()>,
}
#[derive(Default)]
pub struct Runtime {
    pub closing: AtomicBool,
    pub admissions: RwLock<()>,
    pub tasks: Mutex<BTreeMap<String, Task>>,
    pub refresh: Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub rate_next: Mutex<BTreeMap<String, tokio::time::Instant>>,
}
pub fn task_receipt(row: &Value) -> Value {
    let mut out = pick(
        row,
        &[
            "task_id",
            "binding_id",
            "status",
            "progress",
            "total_items",
            "processed_items",
            "started_at",
            "completed_at",
        ],
    );
    let result = ingest::object(&row["result"]);
    out["synced_items"] = result["synced_pages"].clone();
    out["failed_items"] = result["failed_pages"].clone();
    out["error_count"] = result["errors"]
        .as_array()
        .map(|v| json!(v.len()))
        .unwrap_or(Value::Null);
    out["error_summary"] = if row["status"] == "failed" {
        json!("Sync failed or was interrupted. Review source status before retrying.")
    } else {
        Value::Null
    };
    out
}
impl Sources {
    async fn authorize_sync(
        &self,
        user: &query::types::UserContext,
        dataset: &str,
        binding: &str,
        mutation: bool,
    ) -> Result<(query::types::UserContext, Value, Option<Value>)> {
        let actor = self.actor(user, dataset, mutation, mutation).await?;
        let row = self
            .binding(binding)
            .await?
            .ok_or_else(|| Error::missing("Confluence binding not available"))?;
        if !owned(&row, &actor)
            || text(&row, "dataset_id") != dataset
            || text(&row, "created_by") != actor.user_id
        {
            return Err(Error::missing("Confluence binding not available"));
        }
        let connection = if mutation {
            Some(
                self.active_connection(&actor, text(&row, "connection_id"))
                    .await?,
            )
        } else {
            None
        };
        Ok((actor, row, connection))
    }
    pub async fn start_sync(
        self: &Arc<Self>,
        user: &query::types::UserContext,
        dataset: &str,
        binding: &str,
        id: &str,
    ) -> Result<Value> {
        self.start_sync_delta(user, dataset, binding, id, None)
            .await
    }
    pub async fn start_page_sync(
        self: &Arc<Self>,
        user: &query::types::UserContext,
        dataset: &str,
        binding: &str,
        event: &crate::connector::DeltaEvent,
    ) -> Result<Value> {
        if !event.validate("confluence") {
            return Err(Error::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid connector delta identity",
            ));
        }
        // A webhook cannot escape its bound source subtree. New-page events are
        // discovered by the ordinary binding sync before page deltas are accepted.
        self.authorize_sync(user, dataset, binding, true).await?;
        let known: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM confluence_pages WHERE binding_id=$1 AND page_id=$2)",
        )
        .bind(binding)
        .bind(&event.source_id)
        .fetch_one(ai_platform_knowledge_domain::database::executor(&self.pool))
        .await?;
        if !known {
            return Err(Error::missing("Connector page is outside its binding"));
        }
        self.start_sync_delta(
            user,
            dataset,
            binding,
            &event.request_id.to_string(),
            Some(&event.source_id),
        )
        .await
    }
    async fn start_sync_delta(
        self: &Arc<Self>,
        user: &query::types::UserContext,
        dataset: &str,
        binding: &str,
        id: &str,
        page: Option<&str>,
    ) -> Result<Value> {
        let _admission = self.runtime.admissions.read().await;
        if self.runtime.closing.load(Ordering::Acquire) {
            return Err(Error::unavailable(
                "Confluence manual sync is shutting down",
            ));
        }
        let (actor, row, connection) = self.authorize_sync(user, dataset, binding, true).await?;
        let connection = connection.ok_or_else(Error::forbidden)?;
        let mut tx = self.pool.begin().await?;
        ai_platform_knowledge_domain::database::bind_current(&mut tx).await?;
        let current: Value = sqlx::query_scalar(
            "SELECT to_jsonb(b) FROM confluence_space_bindings b WHERE binding_id=$1 FOR UPDATE",
        )
        .bind(binding)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::conflict("Confluence binding changed before admission"))?;
        if pick(&current, BINDING_FIELDS) != pick(&row, BINDING_FIELDS) {
            return Err(Error::conflict(
                "Confluence binding changed before admission",
            ));
        }
        let prior: Option<Value> =
            sqlx::query_scalar("SELECT to_jsonb(t) FROM confluence_sync_tasks t WHERE task_id=$1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(prior) = prior {
            if text(&prior, "binding_id") != binding
                || text(&prior, "owner_id") != actor.user_id
                || prior["page_id"].as_str() != page
            {
                return Err(Error::conflict("Sync request identity already used"));
            }
            return Ok(task_receipt(&prior));
        }
        let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM confluence_sync_tasks WHERE binding_id=$1 AND status IN ('pending','processing'))").bind(binding).fetch_one(&mut *tx).await?;
        if current["status"] == "syncing" || active {
            return Err(Error::conflict(
                "Confluence sync is active or requires interruption reconciliation",
            ));
        }
        sqlx::query("INSERT INTO confluence_sync_tasks(task_id,binding_id,task_type,priority,status,retry_count,max_retries,progress,total_items,processed_items,owner_id,page_id) VALUES($1,$2,$5,1,'pending',0,0,0,0,0,$3,$4)").bind(id).bind(binding).bind(&actor.user_id).bind(page).bind(if page.is_some(){"page_sync"}else{"full_sync"}).execute(&mut *tx).await?;
        sqlx::query("UPDATE confluence_space_bindings SET status='syncing',updated_at=NOW() WHERE binding_id=$1").bind(binding).execute(&mut *tx).await?;
        tx.commit().await?;
        let execution = Execution {
            binding: row,
            connection,
            actor,
            task_id: id.into(),
        };
        let source = self.clone();
        let running = execution.clone();
        let (start, started) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(query::model_plane::inherit(async move {
            if started.await.is_err() {
                return;
            }
            if source.run_sync(&running).await.is_err() {
                let _ = source.interrupted(&running).await;
            }
            source.runtime.tasks.lock().await.remove(&running.task_id);
        }));
        self.runtime
            .tasks
            .lock()
            .await
            .insert(id.into(), Task { execution, handle });
        let _ = start.send(());
        Ok(task_receipt(
            &json!({"task_id":id,"binding_id":binding,"status":"pending"}),
        ))
    }
    pub async fn sync_status(
        &self,
        user: &query::types::UserContext,
        dataset: &str,
        binding: &str,
        id: &str,
    ) -> Result<Value> {
        self.authorize_sync(user, dataset, binding, false).await?;
        let row:Value=sqlx::query_scalar("SELECT to_jsonb(t) FROM confluence_sync_tasks t WHERE task_id=$1 AND binding_id=$2 AND owner_id=$3").bind(id).bind(binding).bind(&user.user_id).fetch_optional(ai_platform_knowledge_domain::database::executor(&self.pool)).await?.ok_or_else(||Error::missing("Confluence sync task not available"))?;
        Ok(task_receipt(&row))
    }
    pub async fn guard(&self, e: &Execution) -> Result<query::types::UserContext> {
        self.guard_deletion(e, None).await
    }
    pub async fn guard_deletion(
        &self,
        e: &Execution,
        deletion: Option<&str>,
    ) -> Result<query::types::UserContext> {
        if self.runtime.closing.load(Ordering::Acquire) {
            return Err(Error::unavailable(
                "Confluence manual sync is shutting down",
            ));
        }
        let actor = self
            .actor(
                &e.actor,
                text(&e.binding, "dataset_id"),
                true,
                deletion.is_none(),
            )
            .await?;
        if let Some(target) = deletion {
            let ds = self
                .store
                .require_dataset_access(&actor, text(&e.binding, "dataset_id"), "editor")
                .await?;
            deletion_writable(
                &serde_json::to_value(ds).map_err(|_| Error::forbidden())?,
                target,
            )?;
        }
        let current = self
            .binding(text(&e.binding, "binding_id"))
            .await?
            .ok_or_else(Error::forbidden)?;
        let connection = self
            .connection(text(&e.binding, "connection_id"))
            .await?
            .ok_or_else(Error::forbidden)?;
        if pick(&current, BINDING_FIELDS) != pick(&e.binding, BINDING_FIELDS)
            || connection_scope(&connection) != connection_scope(&e.connection)
        {
            return Err(Error::forbidden());
        }
        let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM confluence_sync_tasks WHERE task_id=$1 AND binding_id=$2 AND owner_id=$3 AND status IN ('pending','processing'))").bind(&e.task_id).bind(text(&e.binding,"binding_id")).bind(&actor.user_id).fetch_one(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
        if !active {
            return Err(Error::conflict("Confluence task ownership changed"));
        }
        Ok(actor)
    }
    pub async fn guard_on(&self, e: &Execution, conn: &mut PgConnection) -> Result<()> {
        self.guard_deletion_on(e, conn, None).await
    }
    pub async fn guard_deletion_on(
        &self,
        e: &Execution,
        conn: &mut PgConnection,
        deletion: Option<&str>,
    ) -> Result<()> {
        let actor = self
            .actors
            .resolve(&e.actor.tenant_id, &e.actor.user_id)
            .await?;
        if actor.anonymous()
            || actor.user_id != e.actor.user_id
            || actor.tenant_id != e.actor.tenant_id
        {
            return Err(Error::forbidden());
        }
        let ds = locked_dataset(
            conn,
            &actor,
            text(&e.binding, "dataset_id"),
            deletion.is_none(),
        )
        .await?;
        if let Some(target) = deletion {
            deletion_writable(&ds, target)?;
        }
        let binding: Option<Value> = sqlx::query_scalar(
            "SELECT to_jsonb(b) FROM confluence_space_bindings b WHERE binding_id=$1",
        )
        .bind(text(&e.binding, "binding_id"))
        .fetch_optional(ai_platform_knowledge_domain::database::connection(
            &mut *conn,
        ))
        .await?;
        let connection: Option<Value> = sqlx::query_scalar(
            "SELECT to_jsonb(c) FROM confluence_connections c WHERE connection_id=$1",
        )
        .bind(text(&e.binding, "connection_id"))
        .fetch_optional(ai_platform_knowledge_domain::database::connection(
            &mut *conn,
        ))
        .await?;
        if binding.as_ref().map(|b| pick(b, BINDING_FIELDS))
            != Some(pick(&e.binding, BINDING_FIELDS))
            || connection.as_ref().map(connection_scope) != Some(connection_scope(&e.connection))
        {
            return Err(Error::forbidden());
        }
        let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM confluence_sync_tasks WHERE task_id=$1 AND binding_id=$2 AND owner_id=$3 AND status IN ('pending','processing'))").bind(&e.task_id).bind(text(&e.binding,"binding_id")).bind(&actor.user_id).fetch_one(ai_platform_knowledge_domain::database::connection(&mut *conn)).await?;
        if !active {
            return Err(Error::conflict("Confluence task ownership changed"));
        }
        Ok(())
    }
    pub async fn start_acl_refresh(self: &Arc<Self>) -> Result<()> {
        if !self.enabled || !self.refresh_enabled {
            return Ok(());
        }
        if std::env::var_os("KNOWLEDGE_CONNECTOR_PRINCIPALS_FILE").is_none() {
            return Err(Error::unavailable(
                "ACL refresh requires a server principal mapping",
            ));
        }
        let mut running = self.runtime.refresh.lock().await;
        if running.is_some() {
            return Ok(());
        }
        let sources = self.clone();
        *running = Some(tokio::spawn(async move {
            loop {
                if sources.runtime.closing.load(Ordering::Acquire) {
                    break;
                }
                let _ = sources.refresh_one_acl().await;
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
        }));
        Ok(())
    }
    async fn refresh_one_acl(self: &Arc<Self>) -> Result<()> {
        let controlled = ai_platform_knowledge_domain::database::control_enabled(
            &self.pool,
            "knowledge.next_confluence_acl_refresh_v1()",
        )
        .await?;
        let query = if controlled {
            "SELECT * FROM knowledge.next_confluence_acl_refresh_v1()"
        } else {
            "SELECT jsonb_build_object('binding',b.binding_id,'dataset',b.dataset_id,'tenant',b.tenant_id,'actor',b.created_by,'page',p.page_id) FROM confluence_pages p JOIN confluence_space_bindings b ON b.binding_id=p.binding_id JOIN confluence_connections c ON c.connection_id=b.connection_id JOIN knowledge.documents d ON d.document_id=p.document_id AND d.dataset_id=b.dataset_id WHERE b.status<>'syncing' AND b.sync_enabled IS DISTINCT FROM FALSE AND c.status='active' AND CASE WHEN COALESCE(d.metadata->'_connector_acl'->>'expires_at','') ~ '^[0-9]{1,12}$' THEN (d.metadata->'_connector_acl'->>'expires_at')::bigint ELSE 0 END < extract(epoch from clock_timestamp())+900 ORDER BY d.updated_at,p.page_id LIMIT 1"
        };
        let row: Option<Value> = sqlx::query_scalar(query)
            .fetch_optional(ai_platform_knowledge_domain::database::executor(&self.pool))
            .await?;
        let Some(row) = row else { return Ok(()) };
        let actor = self
            .actors
            .resolve(text(&row, "tenant"), text(&row, "actor"))
            .await?;
        let event = crate::connector::DeltaEvent {
            request_id: uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_URL,
                format!(
                    "connector-acl-refresh:{}:{}:{}",
                    text(&row, "binding"),
                    text(&row, "page"),
                    chrono::Utc::now().timestamp() / 300
                )
                .as_bytes(),
            ),
            connector: "confluence".into(),
            source_id: text(&row, "page").into(),
        };
        query::model_plane::with_actor_scope(
            actor.clone(),
            self.start_page_sync(&actor, text(&row, "dataset"), text(&row, "binding"), &event),
        )
        .await?;
        Ok(())
    }
    pub async fn close(&self) -> Result<()> {
        self.runtime.closing.store(true, Ordering::Release);
        if let Some(refresh) = self.runtime.refresh.lock().await.take() {
            refresh.abort();
            let _ = refresh.await;
        }
        let _admissions = self.runtime.admissions.write().await;
        let tasks = std::mem::take(&mut *self.runtime.tasks.lock().await);
        for task in tasks.values() {
            task.handle.abort();
        }
        let mut failure = None;
        for (_, task) in tasks {
            let _ = task.handle.await;
            if let Err(error) = self.interrupted(&task.execution).await {
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    async fn interrupted(&self, e: &Execution) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        ai_platform_knowledge_domain::database::bind_current(&mut tx).await?;
        let changed=sqlx::query("UPDATE confluence_sync_tasks SET status='failed',error='Manual sync interrupted; inspect source status before retrying',completed_at=NOW(),updated_at=NOW() WHERE task_id=$1 AND binding_id=$2 AND owner_id=$3 AND status IN ('pending','processing')").bind(&e.task_id).bind(text(&e.binding,"binding_id")).bind(&e.actor.user_id).execute(&mut *tx).await?.rows_affected();
        if changed > 0 {
            let current:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(b) FROM confluence_space_bindings b WHERE binding_id=$1 FOR UPDATE").bind(text(&e.binding,"binding_id")).fetch_optional(&mut *tx).await?;
            if current.as_ref().is_some_and(|b| {
                pick(b, BINDING_FIELDS) == pick(&e.binding, BINDING_FIELDS)
                    && b["status"] == "syncing"
            }) {
                sqlx::query("UPDATE confluence_space_bindings SET status='error',last_error='Manual sync interrupted; inspect source status before retrying',updated_at=NOW() WHERE binding_id=$1 AND NOT EXISTS(SELECT 1 FROM confluence_sync_tasks WHERE binding_id=$1 AND task_id<>$2 AND status IN ('pending','processing'))").bind(text(&e.binding,"binding_id")).bind(&e.task_id).execute(&mut *tx).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }
    async fn run_sync(&self, e: &Execution) -> Result<()> {
        let actor = self
            .actors
            .resolve(&e.actor.tenant_id, &e.actor.user_id)
            .await?;
        query::model_plane::with_actor_scope(actor, self.run_scoped_sync(e)).await
    }
    async fn run_scoped_sync(&self, e: &Execution) -> Result<()> {
        let actor = self.guard(e).await?;
        let work = self.run_sync_owned(e);
        if let Some(client) = self.ingestion.model_plane.clone() {
            query::model_plane::run_scoped(
                client,
                query::model_plane::ModelResource {
                    dataset_id: Some(text(&e.binding, "dataset_id").into()),
                    document_id: None,
                    execution_id: None,
                },
                actor,
                work,
            )
            .await
        } else {
            query::model_plane::with_actor_scope(actor, work).await
        }
    }
    async fn run_sync_owned(&self, e: &Execution) -> Result<()> {
        self.guard(e).await?;
        let credentials = credentials::open(&e.connection, &self.encryption_key)?;
        let existing: Vec<Value> =
            sqlx::query_scalar("SELECT to_jsonb(p) FROM confluence_pages p WHERE binding_id=$1")
                .bind(text(&e.binding, "binding_id"))
                .fetch_all(ai_platform_knowledge_domain::database::executor(&self.pool))
                .await?;
        let mut roots = e.binding["root_page_ids"]
            .as_array()
            .map(|v| {
                v.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if roots.is_empty() && !text(&e.binding, "root_page_id").is_empty() {
            roots.push(text(&e.binding, "root_page_id").into());
        }
        let delta:Option<String>=sqlx::query_scalar("SELECT page_id FROM confluence_sync_tasks WHERE task_id=$1 AND binding_id=$2 AND owner_id=$3 AND status='pending'").bind(&e.task_id).bind(text(&e.binding,"binding_id")).bind(&e.actor.user_id).fetch_one(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
        let mut pages = Vec::new();
        let mut seen = BTreeSet::new();
        if let Some(page) = &delta {
            pages.push(page.clone());
        } else if roots.is_empty() {
            pages = existing
                .iter()
                .map(|p| text(p, "page_id").to_owned())
                .collect();
        } else {
            let max = e.binding["max_depth"].as_u64().unwrap_or(10);
            let mut queue: VecDeque<_> = roots.iter().map(|r| (r.clone(), 0u64)).collect();
            while let Some((id, depth)) = queue.pop_front() {
                if !seen.insert(id.clone()) {
                    continue;
                }
                pages.push(id.clone());
                if depth >= max {
                    continue;
                }
                self.guard(e).await?;
                // Pagination reads only fixed authenticated origin; each page rechecks source authority.
                let mut cursor = String::new();
                let mut cursors = BTreeSet::new();
                loop {
                    self.guard(e).await?;
                    let mut params = vec![("limit", "100")];
                    if !cursor.is_empty() {
                        params.push(("cursor", cursor.as_str()));
                    }
                    let data = self
                        .provider(&credentials, &["pages", &id, "children"], &params)
                        .await?;
                    let children = data["results"].as_array().ok_or_else(|| {
                        Error::new(
                            StatusCode::BAD_GATEWAY,
                            "Invalid Confluence child discovery response",
                        )
                    })?;
                    for child in children {
                        let id = scalar(&child["id"]);
                        if id.trim().is_empty() {
                            return Err(Error::new(
                                StatusCode::BAD_GATEWAY,
                                "Invalid Confluence child discovery response",
                            ));
                        }
                        queue.push_back((id, depth + 1));
                    }
                    let next = &data["_links"]["next"];
                    if next.is_null() || next == "" {
                        break;
                    }
                    let next = next
                        .as_str()
                        .and_then(|s| {
                            reqwest::Url::parse("https://pagination.invalid/")
                                .ok()?
                                .join(s)
                                .ok()
                        })
                        .ok_or_else(|| {
                            Error::new(
                                StatusCode::BAD_GATEWAY,
                                "Invalid Confluence child pagination response",
                            )
                        })?;
                    cursor = next
                        .query_pairs()
                        .find(|(k, _)| k == "cursor")
                        .map(|(_, v)| v.into_owned())
                        .unwrap_or_default();
                    if cursor.is_empty() || !cursors.insert(cursor.clone()) {
                        return Err(Error::new(
                            StatusCode::BAD_GATEWAY,
                            "Incomplete Confluence child pagination",
                        ));
                    }
                }
            }
        }
        sqlx::query("UPDATE confluence_sync_tasks SET status='processing',total_items=$2,started_at=NOW(),updated_at=NOW() WHERE task_id=$1 AND owner_id=$3 AND status='pending'").bind(&e.task_id).bind(pages.len()as i32).bind(&e.actor.user_id).execute(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
        let mut synced = 0;
        let mut skipped = 0;
        let mut failed = 0;
        let mut errors = Vec::new();
        let mut created = Vec::new();
        let mut updated = Vec::new();
        let mut deleted = Vec::new();
        for (i, id) in pages.iter().enumerate() {
            self.guard(e).await?;
            match self.sync_page(e, &credentials, id).await {
                Ok((document, false, _)) => {
                    skipped += 1;
                    let _ = document;
                }
                Ok((document, true, new)) => {
                    synced += 1;
                    if new {
                        created.push(document);
                    } else {
                        updated.push(document);
                    }
                }
                Err(_) => {
                    failed += 1;
                    errors.push(
                        json!({"page_id":id,"error":"Confluence page synchronization failed"}),
                    );
                }
            }
            sqlx::query("UPDATE confluence_sync_tasks SET processed_items=$2,progress=$3::float8,updated_at=NOW() WHERE task_id=$1 AND owner_id=$4 AND status='processing'").bind(&e.task_id).bind((i+1)as i32).bind((i+1)as f64/pages.len().max(1)as f64*100.).bind(&e.actor.user_id).execute(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
        }
        if delta.is_none() && !roots.is_empty() {
            let discovered: BTreeSet<_> = pages.iter().map(String::as_str).collect();
            for page in existing
                .iter()
                .filter(|p| !discovered.contains(text(p, "page_id")))
            {
                self.guard(e).await?;
                match self.detach_source(e,page).await{Ok(true)=>deleted.push(text(page,"document_id").to_owned()),Ok(false)=>{},Err(_)=>errors.push(json!({"page_id":page["page_id"],"error":"Confluence source removal failed"}))}
            }
        }
        self.guard(e).await?;
        let success = failed == 0 && errors.is_empty();
        let result = json!({"total_pages":pages.len(),"synced_pages":synced,"skipped_pages":skipped,"failed_pages":failed,"created_documents":created,"updated_documents":updated,"deleted_documents":deleted,"errors":errors});
        let mut tx = self.pool.begin().await?;
        ai_platform_knowledge_domain::database::bind_current(&mut tx).await?;
        self.guard_on(e, &mut tx).await?;
        sqlx::query("UPDATE confluence_space_bindings SET status=$2,last_sync_at=CASE WHEN $3 THEN NOW() ELSE last_sync_at END,last_error=$4,synced_page_count=$5,total_page_count=$6,updated_at=NOW() WHERE binding_id=$1").bind(text(&e.binding,"binding_id")).bind(if success{"completed"}else{"error"}).bind(success).bind(if success{None}else{Some("Confluence sync did not complete all pages")}).bind(synced).bind(pages.len()as i32).execute(&mut *tx).await?;
        sqlx::query("UPDATE confluence_sync_tasks SET status=$2,progress=CASE WHEN $3 THEN 100 ELSE progress END,completed_at=NOW(),result=$4,error=$5,updated_at=NOW() WHERE task_id=$1 AND status IN ('pending','processing') AND owner_id=$6").bind(&e.task_id).bind(if success{"completed"}else{"failed"}).bind(success).bind(result).bind(if success{None}else{Some("Confluence sync did not complete all pages")}).bind(&e.actor.user_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}
