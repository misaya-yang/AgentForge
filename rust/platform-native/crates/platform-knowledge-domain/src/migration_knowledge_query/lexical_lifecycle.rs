//! BM25 v2 authority publication and crash-recoverable lexical selection.
use super::{
    lexical_v2::{self, LexicalProfile},
    migration::{SCOPE, SessionLease, digest, matches},
    qdrant::{QdrantStore, id_string},
    store::QueryStore,
    types::{Dataset, QueryError, Result},
};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};
#[derive(Clone)]
pub struct ActivePublicationContext {
    pub dataset: Dataset,
    pub epoch: i64,
    pub profile: LexicalProfile,
}
#[derive(Clone)]
pub struct LexicalLifecycle {
    job_fence: Option<Value>,
    pub store: QueryStore,
    pub vectors: QdrantStore,
    pub barrier_wait: Duration,
    pub quiesce_timeout: Duration,
}
fn conflict(s: &str) -> QueryError {
    QueryError::CollectionAuthority(s.into())
}
fn digest_entries(mut entries: Vec<(String, String)>) -> Result<Value> {
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    if entries.windows(2).any(|a| a[0].0 == a[1].0) {
        return Err(conflict("duplicate lexical point IDs"));
    }
    Ok(
        json!({"point_count":entries.len(),"point_ids_sha256":digest(entries.iter().map(|e|format!("{}\n",e.0)).collect::<String>()),"source_text_sha256":digest(entries.iter().map(|e|format!("{}\0{}\n",e.0,digest(e.1.as_bytes()))).collect::<String>())}),
    )
}
impl LexicalLifecycle {
    pub(crate) fn with_job_fence(mut self, row: Value) -> Self {
        self.job_fence = Some(row);
        self
    }
    async fn check_job_fence(&self, id: &str) -> Result<()> {
        if let Some(row) = &self.job_fence {
            let current = serde_json::to_value(self.store.dataset(id).await?)
                .map_err(|_| QueryError::DependenciesFailed)?;
            crate::migration_knowledge_ingest::vector_jobs::check_expected(row, &current)
                .map_err(|_| QueryError::GenerationChanged)?;
        }
        Ok(())
    }
    pub fn new(pool: PgPool, vectors: QdrantStore) -> Self {
        Self {
            job_fence: None,
            store: QueryStore::new(pool),
            vectors,
            barrier_wait: Duration::from_secs(5),
            quiesce_timeout: Duration::from_secs(60),
        }
    }
    pub async fn state(&self, id: &str) -> Result<Option<Value>> {
        Ok(
            sqlx::query_scalar(
                "SELECT to_jsonb(l) FROM kb_bm25_v2_lifecycle l WHERE dataset_id=$1",
            )
            .bind(id)
            .fetch_optional(crate::database::executor(&self.store.pool))
            .await?,
        )
    }
    pub async fn active_publication_context(
        &self,
        id: &str,
    ) -> Result<Option<ActivePublicationContext>> {
        let dataset = self.store.dataset(id).await?;
        let profile = LexicalProfile::from_dataset(&dataset)?;
        let info = self.vectors.require_collection_readable(&dataset).await?;
        let raw = &info["config"]["metadata"]["knowledge_lexical"];
        let stored = if raw.is_null() {
            None
        } else {
            Some(LexicalProfile::from_collection(raw)?)
        };
        if !profile.active {
            if stored.is_some_and(|p| p.active) {
                return Err(conflict(
                    "collection selects bm25_v2 but PostgreSQL selects lexical_v1",
                ));
            }
            return Ok(None);
        }
        if !self.vectors.bm25_v2_enabled {
            return Err(conflict("bm25_v2 is disabled"));
        }
        let stored = stored
            .filter(|p| p.active)
            .ok_or_else(|| conflict("active lexical selections disagree"))?;
        if profile.schema_fingerprint() != stored.schema_fingerprint()
            || profile.filtering_fingerprint() != stored.filtering_fingerprint()
        {
            return Err(conflict("active lexical fingerprints disagree"));
        }
        let row = self
            .state(id)
            .await?
            .filter(|r| {
                r["state"] == "active_v2"
                    && r["transition_kind"].is_null()
                    && r["lock_token"].is_null()
            })
            .ok_or_else(|| conflict("active lexical lifecycle is not steady"))?;
        Ok(Some(ActivePublicationContext {
            dataset,
            epoch: row["epoch"].as_i64().ok_or(QueryError::Database)?,
            profile,
        }))
    }
    pub async fn authority_on(&self, d: &Dataset, c: &mut PgConnection) -> Result<Value> {
        let row: Option<Value> = sqlx::query_scalar(
            "SELECT to_jsonb(d) FROM datasets d WHERE dataset_id=$1 AND is_deleted=FALSE",
        )
        .bind(&d.dataset_id)
        .fetch_optional(&mut *c)
        .await?;
        let row = row.ok_or(QueryError::NotFound)?;
        if row["tenant_id"] != d.tenant_id
            || row["collection_name"].as_str() != Some(d.collection()?)
            || row["index_config"]["retrieval"]
                .get("_index_deletion_fence")
                .is_some()
        {
            return Err(conflict(
                "lexical authority scope or deletion fence changed",
            ));
        }
        let rows:Vec<(String,String)>=sqlx::query_as("SELECT s.vector_id::text,COALESCE(s.text,'') FROM segments s JOIN documents d ON d.document_id=s.document_id AND d.dataset_id=s.dataset_id JOIN datasets ds ON ds.dataset_id=s.dataset_id WHERE s.dataset_id=$1 AND ds.tenant_id=$2 AND ds.collection_name=$3 AND ds.is_deleted=FALSE AND NOT(COALESCE(ds.index_config->'retrieval','{}'::jsonb) ? '_index_deletion_fence') AND s.vector_id IS NOT NULL AND s.vector_id<>'' AND COALESCE(s.enabled,TRUE)=TRUE AND s.status='completed' AND COALESCE(s.level,3)=3 AND COALESCE(s.content_type,'text')='text' AND COALESCE(d.enabled,TRUE)=TRUE AND COALESCE(d.archived,FALSE)=FALSE AND NOT(COALESCE(d.metadata,'{}'::jsonb) ? '_document_lifecycle_reindex') ORDER BY s.vector_id::text").bind(&d.dataset_id).bind(&d.tenant_id).bind(d.collection()?).fetch_all(&mut *c).await?;
        let mut evidence = digest_entries(rows)?;
        evidence["authority_kind"] = json!("postgres-segments-enabled-l3-text-base-v2");
        evidence["collection_name"] = json!(d.collection()?);
        evidence["dataset_id"] = json!(d.dataset_id);
        evidence["tenant_id"] = json!(d.tenant_id);
        evidence["content_revision"] = json!(row["content_revision"].as_i64().unwrap_or(0));
        Ok(evidence)
    }
    pub async fn authority(&self, d: &Dataset) -> Result<Value> {
        let mut tx = self.store.pool.begin().await?;
        crate::database::bind_current(&mut tx).await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let a = self.authority_on(d, &mut tx).await?;
        tx.commit().await?;
        Ok(a)
    }
    pub async fn scan(&self, d: &Dataset, p: &LexicalProfile) -> Result<Value> {
        let mut entries = Vec::new();
        let mut complete = 0;
        let mut seen = BTreeSet::new();
        for point in self
            .vectors
            .scroll(d, &lexical_v2::lexical_filter(d), true)
            .await?
        {
            let id = id_string(&point["id"]).ok_or(QueryError::VectorStore)?;
            if !seen.insert(id.clone()) {
                return Err(conflict("duplicate lexical point"));
            }
            let payload = &point["payload"];
            if payload["tenant_id"] != d.tenant_id
                || payload["dataset_id"] != d.dataset_id
                || !lexical_v2::eligible(payload)
            {
                return Err(conflict("lexical point escaped scope"));
            }
            let text = payload["text"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| conflict("empty lexical text"))?;
            let marker = p.marker(text);
            let got = &payload["_lexical"];
            if [
                "bm25_v2_schema_fingerprint",
                "filtering_profile_fingerprint",
                "source_text_sha256",
            ]
            .iter()
            .all(|k| got[*k] == marker[*k])
                && got["versions"].as_array().is_some_and(|v| {
                    v.contains(&json!("lexical_v1")) && v.contains(&json!("bm25_v2"))
                })
                && !point["vector"]["bm25_v2"].is_null()
            {
                complete += 1
            }
            entries.push((id, text.into()));
        }
        let mut s = digest_entries(entries)?;
        s["complete_count"] = json!(complete);
        Ok(s)
    }
    fn agreement(a: &Value, s: &Value) -> Result<()> {
        if !matches(a, s, SCOPE) || s["point_count"] != s["complete_count"] {
            Err(conflict(
                "PostgreSQL authority and complete Qdrant lexical scope disagree",
            ))
        } else {
            Ok(())
        }
    }
    fn receipt(
        d: &Dataset,
        p: &LexicalProfile,
        s: &Value,
        revision: i64,
        certified: &str,
    ) -> Result<Value> {
        Ok(
            json!({"schema_version":1,"status":"complete","collection_name":d.collection()?,"bm25_v2_schema_fingerprint":p.schema_fingerprint(),"filtering_profile_fingerprint":p.filtering_fingerprint(),"dataset_id":d.dataset_id,"tenant_id":d.tenant_id,"point_count":s["point_count"],"point_ids_sha256":s["point_ids_sha256"],"manifest_algorithm":"sha256(sorted-point-id-newline-v1)","source_text_sha256":s["source_text_sha256"],"source_text_algorithm":"sha256(sorted-point-id-text-sha256-null-newline-v1)","authority_kind":"postgres-segments-enabled-l3-text-base-v2","authority_content_revision":revision,"certified_by":certified}),
        )
    }
    pub async fn recertify_active_publication(
        &self,
        context: &ActivePublicationContext,
        publication_revision: i64,
        connection: &mut PgConnection,
    ) -> Result<Value> {
        if publication_revision >= 0 {
            return Err(conflict(
                "BM25 certification requires negative publication revision",
            ));
        }
        let mut a = self.authority_on(&context.dataset, connection).await?;
        let revision = a["content_revision"].as_i64().ok_or(QueryError::Database)?;
        if revision >= 0 {
            return Err(conflict(
                "dataset publication fence closed before certification",
            ));
        }
        let target = revision
            .checked_abs()
            .and_then(|v| v.checked_add(1))
            .ok_or(QueryError::Database)?;
        let scope = self.scan(&context.dataset, &context.profile).await?;
        Self::agreement(&a, &scope)?;
        let receipt = Self::receipt(
            &context.dataset,
            &context.profile,
            &scope,
            target,
            "bm25_v2_runtime_publication",
        )?;
        self.vectors
            .publish_lexical_receipt(&context.dataset, &receipt)
            .await?;
        let mut d = context.dataset.clone();
        d.content_revision = Some(target);
        let readiness = self
            .vectors
            .verify_bm25_v2_readiness(&d, &context.profile)
            .await?;
        a["content_revision"] = json!(target);
        Ok(
            json!({"expected_epoch":context.epoch,"target_revision":target,"manifest_sha256":scope["point_ids_sha256"],"post_evidence":{"runtime_publication":true,"authority":a,"readiness":readiness}}),
        )
    }
    pub async fn settle_active_publication(
        &self,
        context: &ActivePublicationContext,
        certification: &Value,
        connection: &mut PgConnection,
    ) -> Result<i64> {
        let revision = certification["target_revision"]
            .as_i64()
            .filter(|r| *r > 0)
            .ok_or_else(|| conflict("publication authority revision must be positive"))?;
        let epoch:Option<i64>=sqlx::query_scalar("UPDATE kb_bm25_v2_lifecycle SET epoch=epoch+1,authority_content_revision=$4,manifest_sha256=$5,post_evidence=$6,last_error=NULL,updated_at=NOW() WHERE dataset_id=$1 AND tenant_id=$2 AND epoch=$3 AND state='active_v2' AND transition_kind IS NULL AND lock_token IS NULL RETURNING epoch").bind(&context.dataset.dataset_id).bind(&context.dataset.tenant_id).bind(certification["expected_epoch"].as_i64()).bind(revision).bind(certification["manifest_sha256"].as_str().unwrap_or("")).bind(&certification["post_evidence"]).fetch_optional(crate::database::connection(&mut *connection)).await?;
        epoch.ok_or_else(|| conflict("active publication epoch CAS failed"))
    }
    pub async fn verify_cross_authority(&self, id: &str) -> Result<Value> {
        let d = self.store.dataset(id).await?;
        let p = LexicalProfile::from_dataset(&d)?;
        let a = self.authority(&d).await?;
        let s = self.scan(&d, &p).await?;
        Ok(
            json!({"dataset_id":id,"agreement":Self::agreement(&a,&s).is_ok(),"postgres_authority":a,"qdrant_scope":s}),
        )
    }
}
impl LexicalLifecycle {
    async fn barrier(&self, id: &str) -> Result<SessionLease> {
        let deadline = Instant::now() + self.barrier_wait;
        loop {
            match SessionLease::acquire(&self.store.pool, format!("knowledge-dataset-index:{id}"))
                .await
            {
                Ok(l) => return Ok(l),
                Err(e) if e.status == 409 && Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(20)).await
                }
                Err(e) => return Err(conflict(&e.message)),
            }
        }
    }
    async fn ensure_recover(&self, d: &Dataset) -> Result<Value> {
        sqlx::query("INSERT INTO kb_bm25_v2_lifecycle(dataset_id,tenant_id) VALUES($1,$2) ON CONFLICT(dataset_id) DO NOTHING").bind(&d.dataset_id).bind(&d.tenant_id).execute(crate::database::executor(&self.store.pool)).await?;
        let actual = self.store.dataset(&d.dataset_id).await?;
        let steady = if LexicalProfile::from_dataset(&actual)?.active {
            "active_v2"
        } else {
            "shadow"
        };
        sqlx::query("UPDATE kb_bm25_v2_lifecycle SET state=$2,epoch=epoch+1,transition_kind=NULL,lock_token=NULL,last_error='reconciled by barrier-winning executor with PostgreSQL lexical profile',updated_at=NOW() WHERE dataset_id=$1 AND state<>$2").bind(&d.dataset_id).bind(steady).execute(crate::database::executor(&self.store.pool)).await?;
        self.state(&d.dataset_id).await?.ok_or(QueryError::Database)
    }
    async fn begin(&self, d: &Dataset, kind: &str, revision: i64) -> Result<(i64, String)> {
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            &uuid::Uuid::new_v4().simple().to_string()[..16]
        );
        let state = format!("{kind}_in_progress");
        let epoch:Option<i64>=sqlx::query_scalar("UPDATE kb_bm25_v2_lifecycle SET state=$3,epoch=epoch+1,transition_kind=$4,lock_token=$5,authority_content_revision=$6,manifest_sha256='',pre_evidence='{}'::jsonb,post_evidence='{}'::jsonb,last_error=NULL,updated_at=NOW() WHERE dataset_id=$1 AND tenant_id=$2 AND state=$7 RETURNING epoch").bind(&d.dataset_id).bind(&d.tenant_id).bind(state).bind(kind).bind(&token).bind(revision).bind(if kind=="cutover"{"shadow"}else{"active_v2"}).fetch_optional(crate::database::executor(&self.store.pool)).await?;
        Ok((
            epoch.ok_or_else(|| conflict("lexical lifecycle begin CAS failed"))?,
            token,
        ))
    }
    async fn settle(
        &self,
        d: &Dataset,
        epoch: i64,
        token: &str,
        kind: &str,
        state: &str,
        pre: Option<Value>,
        post: Option<Value>,
        manifest: &str,
        revision: Option<i64>,
        error: Option<String>,
    ) -> Result<()> {
        let changed=sqlx::query("UPDATE kb_bm25_v2_lifecycle SET state=$3,epoch=epoch+1,transition_kind=NULL,lock_token=NULL,authority_content_revision=COALESCE($7,CASE WHEN $3='shadow' THEN NULL ELSE authority_content_revision END),manifest_sha256=CASE WHEN $8='' THEN manifest_sha256 ELSE $8 END,pre_evidence=COALESCE($5,pre_evidence),post_evidence=COALESCE($6,post_evidence),last_error=$9,updated_at=NOW() WHERE dataset_id=$1 AND epoch=$2 AND lock_token=$4 AND state=$10").bind(&d.dataset_id).bind(epoch).bind(state).bind(token).bind(pre).bind(post).bind(revision).bind(manifest).bind(error.map(|s|s.chars().take(2000).collect::<String>())).bind(format!("{kind}_in_progress")).execute(crate::database::executor(&self.store.pool)).await?.rows_affected();
        if changed != 1 {
            return Err(conflict("lexical lifecycle settlement CAS failed"));
        }
        Ok(())
    }
    async fn flip(
        &self,
        d: &Dataset,
        from: &str,
        to: &str,
        shadow: bool,
        revision: i64,
    ) -> Result<Value> {
        let row:Option<Value>=sqlx::query_scalar("UPDATE datasets SET index_config=jsonb_set(jsonb_set(index_config,'{retrieval,lexical,active_version}',to_jsonb($3::text),FALSE),'{retrieval,lexical,bm25_v2,shadow_write_enabled}',to_jsonb($4::boolean),FALSE),updated_at=NOW() WHERE dataset_id=$1 AND tenant_id=$6 AND is_deleted=FALSE AND content_revision=$5 AND NOT(COALESCE(index_config->'retrieval','{}'::jsonb) ? '_index_deletion_fence') AND index_config#>'{retrieval,lexical}' IS NOT NULL AND COALESCE(index_config#>>'{retrieval,lexical,active_version}','lexical_v1')=$2 RETURNING jsonb_build_object('content_revision',content_revision,'lexical',index_config#>'{retrieval,lexical}')").bind(&d.dataset_id).bind(from).bind(to).bind(shadow).bind(revision).bind(&d.tenant_id).fetch_optional(crate::database::executor(&self.store.pool)).await?;
        row.ok_or_else(|| conflict("dataset lexical revision/selection CAS failed"))
    }
    async fn quiesce(&self, id: &str) -> Result<()> {
        let until = Instant::now() + self.quiesce_timeout;
        loop {
            let n:i64=sqlx::query_scalar("SELECT COUNT(*) FROM documents WHERE dataset_id=$1 AND status NOT IN ('completed','error','waiting')").bind(id).fetch_one(crate::database::executor(&self.store.pool)).await?;
            if n == 0 {
                return Ok(());
            }
            if Instant::now() >= until {
                return Err(QueryError::DependenciesFailed);
            }
            tokio::time::sleep(Duration::from_millis(250)).await
        }
    }
    pub async fn get_lifecycle_state(&self, id: &str) -> Result<Value> {
        let d = self.store.dataset(id).await?;
        let info = self.vectors.collection_info(d.collection()?).await?;
        let busy:i64=sqlx::query_scalar("SELECT COUNT(*) FROM documents WHERE dataset_id=$1 AND status NOT IN ('completed','error','waiting')").bind(id).fetch_one(crate::database::executor(&self.store.pool)).await?;
        let queued:i64=sqlx::query_scalar("SELECT COUNT(*) FROM documents WHERE dataset_id=$1 AND status='waiting' AND NOT(COALESCE(metadata,'{}'::jsonb) ?| ARRAY['_document_upload_generation','_document_upload_failed','_confluence_sync_generation'])").bind(id).fetch_one(crate::database::executor(&self.store.pool)).await?;
        let row = self.state(id).await?;
        Ok(
            json!({"dataset_id":id,"tenant_id":d.tenant_id,"collection_name":d.collection_name,"content_revision":d.content_revision,"state":row.as_ref().and_then(|r|r["state"].as_str()).unwrap_or("shadow"),"lifecycle":row,"postgres_profile":LexicalProfile::from_dataset(&d)?.collection_profile(),"collection_profile":info["config"]["metadata"]["knowledge_lexical"],"receipt_status":info["config"]["metadata"]["knowledge_bm25_v2_backfill"]["status"],"busy_documents":busy,"dispatchable_documents":queued}),
        )
    }
    pub async fn cutover(
        &self,
        id: &str,
        apply: bool,
        expected_manifest: Option<&str>,
    ) -> Result<Value> {
        let d = self.store.dataset(id).await?;
        let p = LexicalProfile::from_dataset(&d)?;
        let info = self.vectors.collection_info(d.collection()?).await?;
        let col =
            LexicalProfile::from_collection(&info["config"]["metadata"]["knowledge_lexical"])?;
        if p.active {
            if !col.active {
                return Err(conflict("active lexical selections disagree"));
            }
            return Ok(
                json!({"dataset_id":id,"applied":false,"already_active":true,"state":"active_v2","collection_name":d.collection_name}),
            );
        }
        if !p.shadow
            || !col.shadow
            || p.schema_fingerprint() != col.schema_fingerprint()
            || p.filtering_fingerprint() != col.filtering_fingerprint()
        {
            return Err(conflict("matching bm25_v2 shadow profile is required"));
        }
        if apply && !self.vectors.bm25_v2_enabled {
            return Err(conflict("bm25_v2 is disabled"));
        }
        if !apply {
            let a = self.authority(&d).await?;
            let s = self.scan(&d, &p).await?;
            Self::agreement(&a, &s)?;
            if expected_manifest.is_some_and(|m| s["point_ids_sha256"] != m) {
                return Err(conflict("lexical manifest does not match"));
            }
            return Ok(
                json!({"applied":false,"dry_run":true,"dataset_id":id,"collection_name":d.collection_name,"verification":{"authority":a,"qdrant_scope":s,"agreement":true}}),
            );
        }
        let lease = self.barrier(id).await?;
        self.check_job_fence(id).await?;
        self.ensure_recover(&d).await?;
        let a = self.authority(&d).await?;
        let revision = a["content_revision"].as_i64().ok_or(QueryError::Database)?;
        let (epoch, token) = self.begin(&d, "cutover", revision).await?;
        let mut pg_flipped = false;
        let mut qdrant_flipped = false;
        let operation:Result<Value>=async{
            self.quiesce(id).await?;let before=self.scan(&d,&p).await?;Self::agreement(&a,&before)?;if expected_manifest.is_some_and(|m|before["point_ids_sha256"]!=m){return Err(conflict("lexical manifest does not match"))}self.vectors.invalidate_lexical_receipt(&d,"bm25_v2_cutover_phase1").await?;let mut active=p.clone();active.active=true;active.shadow=true;self.vectors.select_lexical_profile(&d,&active).await?;qdrant_flipped=true;let scope=self.scan(&d,&active).await?;Self::agreement(&a,&scope)?;let receipt=Self::receipt(&d,&active,&scope,revision,"bm25_v2_lifecycle_cutover")?;self.vectors.publish_lexical_receipt(&d,&receipt).await?;let flipped=self.flip(&d,"lexical_v1","bm25_v2",true,revision).await?;pg_flipped=true;let mut serving=d.clone();serving.index_config["retrieval"]["lexical"]["active_version"]=json!("bm25_v2");let post=self.vectors.verify_bm25_v2_readiness(&serving,&active).await?;if !matches(&a,&post,SCOPE){return Err(conflict("post-cutover readiness differs from authority"))}self.settle(&d,epoch,&token,"cutover","active_v2",Some(json!({"authority":a,"pre_scope":before,"receipt_status_before":info["config"]["metadata"]["knowledge_bm25_v2_backfill"]["status"]})),Some(json!({"readiness":post,"flipped":flipped})),scope["point_ids_sha256"].as_str().unwrap_or(""),Some(revision),None).await?;Ok(json!({"applied":true,"dry_run":false,"dataset_id":id,"collection_name":d.collection_name,"state":"active_v2","epoch":epoch,"manifest_sha256":scope["point_ids_sha256"],"authority_content_revision":revision,"readiness":post}))}.await;
        if let Err(error) = &operation {
            if pg_flipped {
                if let Ok(current) = self.store.dataset(id).await {
                    let _ = self
                        .flip(
                            &current,
                            "bm25_v2",
                            "lexical_v1",
                            true,
                            current.content_revision.unwrap_or(0),
                        )
                        .await;
                }
            }
            if qdrant_flipped {
                let _ = self
                    .vectors
                    .invalidate_lexical_receipt(&d, "bm25_v2_cutover_reverted")
                    .await;
                let _ = self.vectors.select_lexical_profile(&d, &p).await;
            }
            let _ = self
                .settle(
                    &d,
                    epoch,
                    &token,
                    "cutover",
                    "shadow",
                    None,
                    None,
                    "",
                    None,
                    Some(error.to_string()),
                )
                .await;
        }
        lease.release().await.map_err(|e| conflict(&e.message))?;
        operation
    }
    pub async fn rollback(&self, id: &str, apply: bool, keep_shadow: bool) -> Result<Value> {
        let d = self.store.dataset(id).await?;
        let p = LexicalProfile::from_dataset(&d)?;
        let info = self.vectors.collection_info(d.collection()?).await?;
        let mut col =
            LexicalProfile::from_collection(&info["config"]["metadata"]["knowledge_lexical"])?;
        let row = self.state(id).await?;
        if !p.active && row.as_ref().is_none_or(|r| r["state"] == "shadow") {
            if col.active {
                return Err(conflict("split lexical profile requires verification"));
            }
            return Ok(
                json!({"dataset_id":id,"applied":false,"already_inactive":true,"state":"shadow","collection_name":d.collection_name}),
            );
        }
        let keep = keep_shadow && self.vectors.bm25_v2_enabled;
        if !apply {
            return Ok(
                json!({"applied":false,"dry_run":true,"dataset_id":id,"collection_name":d.collection_name,"keep_shadow_writes":keep}),
            );
        }
        let lease = self.barrier(id).await?;
        self.check_job_fence(id).await?;
        let old = self.ensure_recover(&d).await?;
        let a = self.authority(&d).await?;
        let revision = a["content_revision"].as_i64().ok_or(QueryError::Database)?;
        let (epoch, token) = self.begin(&d, "rollback", revision).await?;
        let mut pg_flipped = false;
        let operation:Result<Value>=async{self.vectors.invalidate_lexical_receipt(&d,"bm25_v2_rollback_phase1").await?;self.flip(&d,"bm25_v2","lexical_v1",keep,revision).await?;pg_flipped=true;col.active=false;col.shadow=keep;self.vectors.select_lexical_profile(&d,&col).await?;let serving=self.store.dataset(id).await?;self.vectors.require_collection_readable(&serving).await?;self.settle(&d,epoch,&token,"rollback","shadow",Some(json!({"receipt_status_before":info["config"]["metadata"]["knowledge_bm25_v2_backfill"]["status"]})),Some(json!({"keep_shadow_writes":keep,"v2_field_retained":true})),old["manifest_sha256"].as_str().unwrap_or(""),None,None).await?;Ok(json!({"applied":true,"dry_run":false,"dataset_id":id,"collection_name":d.collection_name,"state":"shadow","epoch":epoch,"keep_shadow_writes":keep,"v2_data_retained":true}))}.await;
        if let Err(error) = &operation {
            if pg_flipped {
                if let Ok(current) = self.store.dataset(id).await {
                    let _ = self
                        .flip(
                            &current,
                            "lexical_v1",
                            "bm25_v2",
                            true,
                            current.content_revision.unwrap_or(0),
                        )
                        .await;
                }
            }
            let _ = self
                .settle(
                    &d,
                    epoch,
                    &token,
                    "rollback",
                    "active_v2",
                    None,
                    None,
                    "",
                    None,
                    Some(error.to_string()),
                )
                .await;
        }
        lease.release().await.map_err(|e| conflict(&e.message))?;
        operation
    }
}
