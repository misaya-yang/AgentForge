//! PostgreSQL-authoritative embedding blue/green lifecycle. Remote vectors never
//! witness their own completeness, and only a fresh pinned gate permits cutover.
use super::{
    config::RetrieveRequest,
    embedding::EmbeddingManager,
    lexical,
    qdrant::{QdrantStore, id_string},
    store::QueryStore,
    types::{Dataset, QueryError},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection, PgPool, Postgres, pool::PoolConnection};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
#[derive(Debug, Clone)]
pub struct MigrationError {
    pub status: u16,
    pub message: String,
}
pub type MResult<T> = std::result::Result<T, MigrationError>;
impl MigrationError {
    pub fn conflict(s: impl Into<String>) -> Self {
        Self {
            status: 409,
            message: s.into(),
        }
    }
    pub fn invalid(s: impl Into<String>) -> Self {
        Self {
            status: 400,
            message: s.into(),
        }
    }
}
impl From<sqlx::Error> for MigrationError {
    fn from(e: sqlx::Error) -> Self {
        if e.as_database_error()
            .is_some_and(|e| e.code().as_deref() == Some("23505"))
        {
            Self::conflict("embedding binding or action already reserved")
        } else {
            Self {
                status: 503,
                message: "embedding versioning store unavailable".into(),
            }
        }
    }
}
impl From<QueryError> for MigrationError {
    fn from(e: QueryError) -> Self {
        Self {
            status: match e {
                QueryError::Forbidden => 403,
                QueryError::NotFound => 404,
                QueryError::Validation(_) => 400,
                QueryError::PublicationInProgress | QueryError::GenerationChanged => 409,
                _ => 503,
            },
            message: e.to_string(),
        }
    }
}
pub fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
pub const LIVE: &[&str] = &["shadow_build", "backfilling", "verified", "gating", "ready"];
pub const SCOPE: &[&str] = &["point_count", "point_ids_sha256", "source_text_sha256"];
pub const AUTHORITY: &[&str] = &[
    "authority_kind",
    "dataset_id",
    "tenant_id",
    "content_revision",
    "point_count",
    "point_ids_sha256",
    "source_text_sha256",
];
const ENABLED: &str = " FROM segments s JOIN documents d ON d.document_id=s.document_id WHERE s.dataset_id=$1 AND s.enabled=TRUE AND d.enabled=TRUE AND COALESCE(d.archived,FALSE)=FALSE AND COALESCE(s.content_type,'text')='text' ";
const PENDING: &str = " FROM segments s JOIN documents d ON d.document_id=s.document_id LEFT JOIN embedding_migration_progress p ON p.migration_id=$1::uuid AND p.segment_id=s.segment_id WHERE s.dataset_id=$2 AND s.enabled=TRUE AND d.enabled=TRUE AND COALESCE(d.archived,FALSE)=FALSE AND COALESCE(s.content_type,'text')='text' AND (p.segment_id IS NULL OR p.content_hash IS DISTINCT FROM COALESCE(s.content_hash,s.index_node_hash,'')) ";
pub fn matches(a: &Value, b: &Value, keys: &[&str]) -> bool {
    a.is_object()
        && b.is_object()
        && keys
            .iter()
            .all(|k| a.get(*k).is_some() && a.get(*k) == b.get(*k))
}
pub fn identity(value: &Value) -> Value {
    json!({"embedding_provider":text(value,"embedding_provider").trim(),"embedding_model":text(value,"embedding_model").trim(),"embedding_model_version":text(value,"embedding_model_version").trim(),"embedding_dimension":value["embedding_dimension"].as_i64().unwrap_or(0)})
}
pub fn sanitize(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        let c = if c.is_ascii_alphanumeric() || c == '-' {
            c
        } else {
            '_'
        };
        if c != '_' || !out.ends_with('_') {
            out.push(c)
        }
    }
    let out = out.trim_matches('_');
    if out.is_empty() {
        "kb".into()
    } else {
        out.into()
    }
}
pub fn digest(s: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(s.as_ref()))
}
fn scope_evidence(entries: &[(String, String)]) -> MResult<Value> {
    let mut entries = entries.to_vec();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    if entries.windows(2).any(|p| p[0].0 == p[1].0) {
        return Err(MigrationError::conflict("duplicate vector IDs"));
    }
    let ids = entries
        .iter()
        .map(|e| format!("{}\n", e.0))
        .collect::<String>();
    let texts = entries
        .iter()
        .map(|e| format!("{}\0{}\n", e.0, digest(e.1.as_bytes())))
        .collect::<String>();
    Ok(
        json!({"point_count":entries.len(),"point_ids_sha256":digest(ids),"source_text_sha256":digest(texts)}),
    )
}
/// A session lock spans HTTP evidence collection. Cancellation closes the
/// connection instead of returning a still-locked session to the pool.
pub struct SessionLease {
    connection: Option<PoolConnection<Postgres>>,
    name: String,
    shared: bool,
}
impl SessionLease {
    pub async fn acquire(pool: &PgPool, name: String) -> MResult<Self> {
        let mut c = pool.acquire().await?;
        let held: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtextextended($1,0))")
            .bind(&name)
            .fetch_one(&mut *c)
            .await?;
        if !held {
            return Err(MigrationError::conflict("dataset or migration is busy"));
        }
        Ok(Self {
            connection: Some(c),
            name,
            shared: false,
        })
    }
    pub fn connection(&mut self) -> &mut PgConnection {
        self.connection.as_mut().expect("live lease")
    }
    pub async fn release(mut self) -> MResult<()> {
        if let Some(mut c) = self.connection.take() {
            let result: std::result::Result<bool, sqlx::Error> =
                sqlx::query_scalar(if self.shared {
                    "SELECT pg_advisory_unlock_shared(hashtextextended($1,0))"
                } else {
                    "SELECT pg_advisory_unlock(hashtextextended($1,0))"
                })
                .bind(&self.name)
                .fetch_one(&mut *c)
                .await;
            let unlocked = match result {
                Ok(value) => value,
                Err(e) => {
                    c.close_on_drop();
                    return Err(e.into());
                }
            };
            if !unlocked {
                c.close_on_drop();
                return Err(MigrationError::conflict("dataset lease was lost"));
            }
        }
        Ok(())
    }
}
impl Drop for SessionLease {
    fn drop(&mut self) {
        if let Some(c) = self.connection.as_mut() {
            c.close_on_drop()
        }
    }
}
#[derive(Clone)]
pub struct MigrationStore {
    pub pool: PgPool,
}
impl MigrationStore {
    pub async fn migration(&self, id: &str) -> MResult<Value> {
        if uuid::Uuid::parse_str(id).is_err() {
            return Err(MigrationError {
                status: 404,
                message: "migration not found".into(),
            });
        }
        let row: Option<Value> = sqlx::query_scalar(
            "SELECT to_jsonb(m) FROM embedding_migrations m WHERE migration_id=$1::uuid",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.ok_or_else(|| MigrationError {
            status: 404,
            message: "migration not found".into(),
        })
    }
    async fn optional_binding(&self, id: &str) -> MResult<Option<Value>> {
        if uuid::Uuid::parse_str(id).is_err() {
            return Ok(None);
        }
        Ok(sqlx::query_scalar(
            "SELECT to_jsonb(b) FROM dataset_collection_bindings b WHERE binding_id=$1::uuid",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }
    pub async fn binding(&self, id: &str) -> MResult<Value> {
        self.optional_binding(id)
            .await?
            .ok_or_else(|| MigrationError::conflict("embedding binding missing"))
    }
    pub async fn transition(
        &self,
        id: &str,
        from: &[&str],
        to: &str,
        gate: Option<&Value>,
        error: Option<&str>,
    ) -> MResult<Value> {
        let from: Vec<String> = from.iter().map(|s| s.to_string()).collect();
        let row:Option<Value>=sqlx::query_scalar("UPDATE embedding_migrations SET state=$3,gate=COALESCE($4::jsonb,gate),error=$5,updated_at=NOW() WHERE migration_id=$1::uuid AND state=ANY($2::text[]) RETURNING to_jsonb(embedding_migrations)").bind(id).bind(from).bind(to).bind(gate).bind(error).fetch_optional(&self.pool).await?;
        row.ok_or_else(|| MigrationError::conflict(format!("migration cannot transition to {to}")))
    }
    pub async fn merge(&self, id: &str, checkpoint: Value, totals: Value) -> MResult<()> {
        sqlx::query("UPDATE embedding_migrations SET checkpoint=checkpoint||$2::jsonb,totals=totals||$3::jsonb,updated_at=NOW() WHERE migration_id=$1::uuid").bind(id).bind(checkpoint).bind(totals).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn pending_on(c: &mut PgConnection, id: &str, dataset: &str) -> MResult<i64> {
        Ok(sqlx::query_scalar(&format!("SELECT COUNT(*){PENDING}"))
            .bind(id)
            .bind(dataset)
            .fetch_one(c)
            .await?)
    }
    pub async fn pending(&self, id: &str, dataset: &str) -> MResult<i64> {
        let mut c = self.pool.acquire().await?;
        Self::pending_on(&mut c, id, dataset).await
    }
    pub async fn authority_on(c: &mut PgConnection, id: &str) -> MResult<Value> {
        let d: Option<Value> = sqlx::query_scalar(
            "SELECT to_jsonb(d) FROM datasets d WHERE dataset_id=$1 AND is_deleted=FALSE",
        )
        .bind(id)
        .fetch_optional(&mut *c)
        .await?;
        let d = d.ok_or_else(|| MigrationError::conflict("authoritative dataset missing"))?;
        let revision = d["content_revision"].as_i64().unwrap_or(0);
        if revision < 0 {
            return Err(MigrationError::conflict(
                "dataset publication is in progress",
            ));
        }
        let entries:Vec<(String,String)>=sqlx::query_as(&format!("SELECT COALESCE(NULLIF(s.vector_id,''),s.segment_id),COALESCE(s.text,''){ENABLED} ORDER BY COALESCE(NULLIF(s.vector_id,''),s.segment_id)")).bind(id).fetch_all(&mut *c).await?;
        let mut evidence = scope_evidence(&entries)?;
        for (k, v) in [
            ("authority_kind", json!("postgres-enabled-text-segments-v1")),
            ("dataset_id", json!(id)),
            ("tenant_id", d["tenant_id"].clone()),
            ("serving_collection_name", d["collection_name"].clone()),
            ("content_revision", json!(revision)),
        ] {
            evidence[k] = v
        }
        Ok(evidence)
    }
    pub async fn authority(&self, id: &str) -> MResult<Value> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let e = Self::authority_on(&mut tx, id).await?;
        tx.commit().await?;
        Ok(e)
    }
    pub async fn serving(&self, d: &Dataset) -> MResult<Value> {
        let mut tx = self.pool.begin().await?;
        dataset_lock(&mut tx, &d.dataset_id).await?;
        let current:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(b) FROM dataset_collection_bindings b WHERE dataset_id=$1 AND state='serving'").bind(&d.dataset_id).fetch_optional(&mut *tx).await?;
        if let Some(current) = current {
            tx.commit().await?;
            return Ok(current);
        }
        let identity = identity(&d.to_value());
        if text(&identity, "embedding_provider").is_empty()
            || text(&identity, "embedding_model").is_empty()
            || identity["embedding_dimension"].as_i64().unwrap_or(0) <= 0
        {
            return Err(MigrationError::invalid(
                "dataset has no adoptable embedding identity",
            ));
        }
        let b = insert_binding(
            &mut tx,
            d,
            d.collection()?,
            &identity,
            "serving",
            &json!([]),
        )
        .await?;
        tx.commit().await?;
        Ok(b)
    }
}
pub async fn dataset_lock(c: &mut PgConnection, id: &str) -> MResult<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("knowledge-dataset-index:{id}"))
        .execute(c)
        .await?;
    Ok(())
}
async fn insert_binding(
    c: &mut PgConnection,
    d: &Dataset,
    name: &str,
    i: &Value,
    state: &str,
    caps: &Value,
) -> MResult<Value> {
    Ok(sqlx::query_scalar("INSERT INTO dataset_collection_bindings(dataset_id,tenant_id,collection_name,embedding_provider,embedding_model,embedding_model_version,embedding_dimension,capabilities,state,activated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,CASE WHEN $9='serving' THEN NOW() END) RETURNING to_jsonb(dataset_collection_bindings)").bind(&d.dataset_id).bind(&d.tenant_id).bind(name).bind(text(i,"embedding_provider")).bind(text(i,"embedding_model")).bind(text(i,"embedding_model_version")).bind(i["embedding_dimension"].as_i64().unwrap_or(0)as i32).bind(caps).bind(state).fetch_one(c).await?)
}
fn binding_dataset(base: &Dataset, b: &Value) -> Dataset {
    let mut d = base.clone();
    d.collection_name = Some(text(b, "collection_name").into());
    d.embedding_provider = Some(text(b, "embedding_provider").into());
    d.embedding_model = Some(text(b, "embedding_model").into());
    d.embedding_model_version = Some(text(b, "embedding_model_version").into());
    d.embedding_dimension = b["embedding_dimension"].as_u64().map(|n| n as usize);
    d
}
#[derive(Clone)]
pub struct EmbeddingMigration {
    pub store: MigrationStore,
    pub query: QueryStore,
    pub vectors: QdrantStore,
    pub embeddings: EmbeddingManager,
}
impl EmbeddingMigration {
    pub fn new(pool: PgPool, vectors: QdrantStore, embeddings: EmbeddingManager) -> Self {
        Self {
            store: MigrationStore { pool: pool.clone() },
            query: QueryStore::new(pool),
            vectors,
            embeddings,
        }
    }
    pub async fn start(
        &self,
        d: &Dataset,
        target: Value,
        capabilities: Option<Value>,
        tag: &str,
    ) -> MResult<Value> {
        let i = identity(&target);
        let tag = if tag.is_empty() {
            let spaced = crate::migration_content::python_json(&i, true, true);
            digest(spaced.as_bytes())[..8].to_owned()
        } else {
            tag.to_string()
        };
        let name = format!(
            "kb_{}_{}_v{}",
            sanitize(&d.dataset_id),
            i["embedding_dimension"],
            sanitize(&tag)
        );
        // Serialize ensure/reserve/compensation by physical collection name.
        // A racing start must never lose its newly bound collection to cleanup.
        let lease = SessionLease::acquire(
            &self.store.pool,
            format!("knowledge-embedding-start:{name}"),
        )
        .await?;
        let result = self.start_reserved(d, target, capabilities, &name).await;
        lease.release().await?;
        result
    }
    async fn start_reserved(
        &self,
        d: &Dataset,
        target: Value,
        capabilities: Option<Value>,
        name: &str,
    ) -> MResult<Value> {
        let i = identity(&target);
        if text(&i, "embedding_provider").is_empty()
            || text(&i, "embedding_model").is_empty()
            || !i["embedding_dimension"]
                .as_u64()
                .is_some_and(|n| (1..=8192).contains(&n))
        {
            return Err(MigrationError::invalid(
                "target embedding identity is incomplete",
            ));
        }
        let source = self.store.serving(d).await?;
        if identity(&source) == i {
            return Err(MigrationError::invalid(
                "target embedding identity is unchanged",
            ));
        }
        for suffix in ["_summary", "_sections"] {
            if self
                .vectors
                .collection_exists(&format!("{}{suffix}", text(&source, "collection_name")))
                .await?
            {
                return Err(MigrationError::invalid(
                    "hierarchical auxiliary collections cannot be migrated",
                ));
            }
        }
        let existing:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(b) FROM dataset_collection_bindings b WHERE collection_name=$1 AND state<>'retired'").bind(name).fetch_optional(&self.store.pool).await?;
        if let Some(b) = &existing {
            if b["dataset_id"] != d.dataset_id || b["tenant_id"] != d.tenant_id {
                return Err(MigrationError::conflict(
                    "shadow collection belongs to another dataset",
                ));
            }
            if identity(b) != i {
                return Err(MigrationError::conflict("shadow identity conflicts"));
            }
            let prior:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(m) FROM embedding_migrations m WHERE target_binding_id=$1::uuid ORDER BY created_at DESC LIMIT 1").bind(text(b,"binding_id")).fetch_optional(&self.store.pool).await?;
            if let Some(p) = prior {
                let state = text(&p, "state");
                if ["failed", "gate_failed"].contains(&state) {
                    let busy:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM embedding_migrations WHERE dataset_id=$1 AND state=ANY($2::text[]))").bind(&d.dataset_id).bind(LIVE).fetch_one(&self.store.pool).await?;
                    if busy {
                        return Err(MigrationError::conflict(
                            "dataset already has a live migration",
                        ));
                    }
                    return Ok(
                        json!({"migration":p,"serving_binding":source,"target_binding":b,"resumed":true}),
                    );
                }
                if state == "rolled_back" && b["state"] == "shadow" {
                    let mut tx = self.store.pool.begin().await?;
                    let current = locked_migration(&mut tx, text(&p, "migration_id")).await?;
                    dataset_lock(&mut tx, &d.dataset_id).await?;
                    let retained =
                        locked_binding(&mut tx, text(&current, "target_binding_id")).await?;
                    let serving =
                        locked_binding(&mut tx, text(&current, "source_binding_id")).await?;
                    if current["state"] != "rolled_back"
                        || retained["state"] != "shadow"
                        || serving["state"] != "serving"
                    {
                        return Err(MigrationError::conflict(
                            "rolled-back migration no longer has retryable serving and shadow bindings",
                        ));
                    }
                    let busy: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM embedding_migrations WHERE dataset_id=$1 AND state=ANY($2::text[]))")
                        .bind(&d.dataset_id).bind(LIVE).fetch_one(&mut *tx).await?;
                    if busy {
                        return Err(MigrationError::conflict(
                            "cannot reopen migration: dataset already has a live embedding migration",
                        ));
                    }
                    let m: Value = sqlx::query_scalar("UPDATE embedding_migrations SET state='backfilling',error=NULL,updated_at=NOW() WHERE migration_id=$1::uuid AND state='rolled_back' RETURNING to_jsonb(embedding_migrations)")
                        .bind(text(&p,"migration_id")).fetch_one(&mut *tx).await?;
                    tx.commit().await?;
                    return Ok(
                        json!({"migration":m,"serving_binding":source,"target_binding":b,"resumed":true,"reopened":true}),
                    );
                }
                return Err(MigrationError::conflict(
                    "shadow reservation must be aborted before reuse",
                ));
            }
        }
        let target_dataset = binding_dataset(
            d,
            &json!({"collection_name":name,"embedding_provider":i["embedding_provider"],"embedding_model":i["embedding_model"],"embedding_model_version":i["embedding_model_version"],"embedding_dimension":i["embedding_dimension"]}),
        );
        let existed = self.vectors.collection_exists(name).await?;
        if existed
            && self.vectors.collection_info(name).await?["config"]["params"]["vectors"]["size"]
                .as_u64()
                != i["embedding_dimension"].as_u64()
        {
            // Avoid ensure_collection's general dimension-suffix fallback:
            // this operation reserves one deterministic generation name.
            return Err(MigrationError::conflict(
                "shadow collection dimension mismatch",
            ));
        }
        let actual = self
            .vectors
            .ensure_collection(&target_dataset, target_dataset.embedding_dimension.unwrap())
            .await?;
        if actual != name {
            return Err(MigrationError::conflict(
                "shadow collection dimension mismatch",
            ));
        }
        let opened: MResult<Value> = async {
        let mut tx = self.store.pool.begin().await?;
        dataset_lock(&mut tx, &d.dataset_id).await?;
        let busy:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM embedding_migrations WHERE dataset_id=$1 AND state=ANY($2::text[]))").bind(&d.dataset_id).bind(LIVE).fetch_one(&mut *tx).await?;
        if busy {
            return Err(MigrationError::conflict(
                "dataset already has a live migration",
            ));
        }
        let caps = capabilities.unwrap_or_else(|| source["capabilities"].clone());
        let current_source = locked_binding(&mut tx, text(&source,"binding_id")).await?;
        if current_source["state"] != "serving" {
            return Err(MigrationError::conflict("dataset serving generation changed"));
        }
        let target = match existing {
            Some(b) => {
                let b = locked_binding(&mut tx, text(&b,"binding_id")).await?;
                if b["state"] != "shadow" {
                    return Err(MigrationError::conflict("orphan binding is no longer a shadow"));
                }
                b
            },
            None => insert_binding(&mut tx, d, &name, &i, "shadow", &caps).await?,
        };
        let migration:Value=sqlx::query_scalar("INSERT INTO embedding_migrations(dataset_id,source_binding_id,target_binding_id,state,checkpoint,totals) VALUES($1,$2::uuid,$3::uuid,'shadow_build',$4,$5) RETURNING to_jsonb(embedding_migrations)").bind(&d.dataset_id).bind(text(&source,"binding_id")).bind(text(&target,"binding_id")).bind(json!({"source_identity":identity(&source)})).bind(json!({"target_collection":name})).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(json!({"migration":migration,"serving_binding":source,"target_binding":target}))
        }.await;
        if opened.is_err() && !existed {
            // Fail closed on a failed reference lookup, including uncertain
            // commits. Only a proven unbound collection is an orphan.
            let referenced: std::result::Result<bool, sqlx::Error> = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dataset_collection_bindings WHERE collection_name=$1 AND state<>'retired')")
                .bind(name).fetch_one(&self.store.pool).await;
            if matches!(referenced, Ok(false)) {
                let _ = self.vectors.delete_collection(&target_dataset).await;
            }
        }
        opened
    }
    async fn context(&self, id: &str) -> MResult<(Value, Dataset, Value, Value)> {
        let m = self.store.migration(id).await?;
        let d = self.query.dataset(text(&m, "dataset_id")).await?;
        let source = self.store.binding(text(&m, "source_binding_id")).await?;
        let target = self.store.binding(text(&m, "target_binding_id")).await?;
        Ok((m, d, source, target))
    }
    pub async fn scan_target(&self, d: &Dataset, b: &Value) -> MResult<Value> {
        if d.dataset_id.trim().is_empty()
            || d.tenant_id.trim().is_empty()
            || text(b, "collection_name").trim().is_empty()
            || text(b, "embedding_model").trim().is_empty()
            || b["embedding_dimension"].as_u64().unwrap_or(0) == 0
            || b["dataset_id"] != d.dataset_id
            || b["tenant_id"] != d.tenant_id
        {
            return Err(MigrationError::invalid(
                "embedding migration scope needs collection, tenant, dataset, model, and a positive dimension",
            ));
        }
        // Canonical scope scans omit vectors: Qdrant enforces their dimension
        // on write, while the uncached scan verifies identity and source text.
        let target = binding_dataset(d, b);
        let points = self.vectors.scroll_all(&target).await?;
        let mut entries = Vec::new();
        for p in points {
            let payload = &p["payload"];
            if payload["dataset_id"] != d.dataset_id
                || payload["tenant_id"] != d.tenant_id
                || payload["embedding_model"] != b["embedding_model"]
                || text(payload, "embedding_model_version") != text(b, "embedding_model_version")
            {
                return Err(MigrationError::conflict(
                    "shadow collection contains foreign scope or embedding provenance",
                ));
            }
            let id = id_string(&p["id"])
                .ok_or_else(|| MigrationError::conflict("shadow point has invalid ID"))?;
            let text = payload["text"]
                .as_str()
                .ok_or_else(|| MigrationError::conflict("shadow point has no source text"))?;
            entries.push((id, text.to_string()));
        }
        scope_evidence(&entries)
    }
    async fn evidence(&self, id: &str) -> MResult<(Value, Value)> {
        let (_, d, _, target) = self.context(id).await?;
        let pending = self.store.pending(id, &d.dataset_id).await?;
        let authority = self.store.authority(&d.dataset_id).await?;
        let scope = self.scan_target(&d, &target).await?;
        if pending != 0 {
            return Err(MigrationError::invalid(format!(
                "{pending} enabled chunks are not yet embedded into the shadow collection"
            )));
        }
        if !matches(&authority, &scope, SCOPE) {
            return Err(MigrationError::invalid(
                "shadow collection does not match the PostgreSQL authority (point/source digest mismatch)",
            ));
        }
        Ok((authority, scope))
    }
    pub async fn backfill(&self, id: &str, page_size: usize, max_rounds: usize) -> MResult<Value> {
        let lease = SessionLease::acquire(
            &self.store.pool,
            format!("knowledge-embedding-backfill:{id}"),
        )
        .await?;
        let result = self
            .backfill_claimed(id, page_size.clamp(1, 2000), max_rounds.max(1))
            .await;
        if result.is_err() {
            let _ = self
                .store
                .transition(
                    id,
                    &["shadow_build", "backfilling"],
                    "failed",
                    None,
                    Some("embedding backfill failed"),
                )
                .await;
        }
        lease.release().await?;
        result
    }
    async fn backfill_claimed(
        &self,
        id: &str,
        page_size: usize,
        max_rounds: usize,
    ) -> MResult<Value> {
        let (_, d, _, b) = self.context(id).await?;
        self.store
            .transition(
                id,
                &["shadow_build", "backfilling", "failed"],
                "backfilling",
                None,
                None,
            )
            .await?;
        let target = binding_dataset(&d, &b);
        let resolved = self.embeddings.resolve(&target).await?;
        let mut embedded = 0;
        let mut rounds = 0;
        for round in 0..max_rounds {
            let rows:Vec<Value>=sqlx::query_scalar(&format!("SELECT to_jsonb(s)||jsonb_build_object('vector_id',COALESCE(NULLIF(s.vector_id,''),s.segment_id),'content_hash',COALESCE(s.content_hash,s.index_node_hash,'')){PENDING} ORDER BY s.document_id,s.position LIMIT $3")).bind(id).bind(&d.dataset_id).bind(page_size as i64).fetch_all(&self.store.pool).await?;
            if rows.is_empty() {
                break;
            }
            let hashes: Vec<_> = rows
                .iter()
                .map(|r| text(r, "content_hash").to_owned())
                .collect();
            let cache = self
                .query
                .embedding_cache(&hashes, &b)
                .await
                .unwrap_or_default();
            let mut vectors = Vec::<Option<Vec<f64>>>::new();
            let mut misses = Vec::new();
            let mut positions = Vec::new();
            for (index, row) in rows.iter().enumerate() {
                let v = cache
                    .get(text(row, "content_hash"))
                    .filter(|v| v.len() == resolved.dimension)
                    .cloned();
                if v.is_none() {
                    positions.push(index);
                    misses.push(text(row, "text").into())
                }
                vectors.push(v)
            }
            let fresh = self
                .embeddings
                .embed_resolved(&resolved, &misses, "document")
                .await?;
            if fresh.len() != misses.len() {
                return Err(MigrationError::invalid("embedding count mismatch"));
            }
            embedded += fresh.len();
            for (index, vector) in positions.into_iter().zip(fresh) {
                if vector.len() != resolved.dimension {
                    return Err(MigrationError::invalid("embedding dimension mismatch"));
                }
                vectors[index] = Some(vector)
            }
            let mut points = Vec::new();
            for (row, vector) in rows.iter().zip(vectors) {
                let vector = vector.ok_or_else(|| MigrationError::invalid("missing vector"))?;
                let hash = text(row, "content_hash");
                let _=sqlx::query("INSERT INTO embedding_vector_cache(embedding_provider,embedding_model,embedding_model_version,content_hash,vector,dimension) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(embedding_provider,embedding_model,embedding_model_version,content_hash) DO UPDATE SET vector=EXCLUDED.vector,dimension=EXCLUDED.dimension,updated_at=NOW()").bind(text(&b,"embedding_provider")).bind(text(&b,"embedding_model")).bind(text(&b,"embedding_model_version")).bind(hash).bind(&vector).bind(resolved.dimension as i32).execute(&self.store.pool).await;
                let mut metadata = json!({});
                for k in [
                    "source_type",
                    "citation_text",
                    "source_reference",
                    "section_title",
                    "section_full_path",
                    "page_number",
                    "chunk_index",
                    "paragraph_index",
                    "source_document",
                    "document_title",
                    "madhab",
                    "language",
                ] {
                    if let Some(v) = row["metadata"].get(k) {
                        metadata[k] = v.clone()
                    }
                }
                let mut payload = json!({"tenant_id":d.tenant_id,"dataset_id":d.dataset_id,"segment_id":row["segment_id"],"document_id":row["document_id"],"position":row["position"],"text":row["text"],"token_count":row["token_count"],"embedding_model":b["embedding_model"],"embedding_model_version":b["embedding_model_version"],"content_hash":hash,"metadata":metadata});
                for k in [
                    "source_type",
                    "language",
                    "citation_text",
                    "source_reference",
                ] {
                    payload[k] = metadata[k].clone()
                }
                let (indices, values) = lexical::sparse_vector(text(row, "text"), false);
                let (indices, values) = if indices.is_empty() {
                    (vec![0], vec![1.0])
                } else {
                    (indices, values)
                };
                points.push(json!({"id":row["vector_id"],"vector":{"":vector,"bm25":{"indices":indices,"values":values}},"payload":payload}));
            }
            let write_lease = SessionLease::acquire_shared(
                &self.store.pool,
                format!("knowledge-dataset-index:{}", d.dataset_id),
            )
            .await?;
            self.vectors.upsert_index_points(&target, &points).await?;
            write_lease.release().await?;
            let mut tx = self.store.pool.begin().await?;
            for row in &rows {
                sqlx::query("INSERT INTO embedding_migration_progress(migration_id,segment_id,document_id,position,vector_id,content_hash) VALUES($1::uuid,$2,$3,$4,$5,$6) ON CONFLICT(migration_id,segment_id) DO UPDATE SET content_hash=EXCLUDED.content_hash,vector_id=EXCLUDED.vector_id,written_at=NOW()").bind(id).bind(text(row,"segment_id")).bind(text(row,"document_id")).bind(row["position"].as_i64().unwrap_or(0)as i32).bind(text(row,"vector_id")).bind(text(row,"content_hash")).execute(&mut *tx).await?;
            }
            tx.commit().await?;
            rounds = round + 1;
            self.store
                .merge(
                    id,
                    json!({"last_round":rounds,"page_size":page_size}),
                    json!({"embedded_in_run":embedded}),
                )
                .await?;
        }
        let total: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*){ENABLED}"))
            .bind(&d.dataset_id)
            .fetch_one(&self.store.pool)
            .await?;
        Ok(
            json!({"migration":self.store.migration(id).await?,"embedded":embedded,"enabled_chunks":total,"remaining":self.store.pending(id,&d.dataset_id).await?,"rounds":rounds}),
        )
    }
    pub async fn verify(&self, id: &str, job: &str) -> MResult<Value> {
        let (a, s) = self.evidence(id).await?;
        self.store.merge(id,json!({}),json!({"verified_enabled_chunks":a["point_count"],"verified_points":s["point_count"],"verified_authority":a,"verified_target_scope":s,"verify_action_job_id":job})).await?;
        let m = self
            .store
            .transition(
                id,
                &["backfilling", "verified", "failed"],
                "verified",
                None,
                None,
            )
            .await?;
        Ok(
            json!({"migration":m,"enabled_chunks":a["point_count"],"points":s["point_count"],"authority_snapshot":a,"target_scope":s}),
        )
    }
    async fn fail_gate(&self, id: &str, verdict: Value, message: &str) -> MResult<Value> {
        self.store
            .transition(id, &["gating"], "gate_failed", Some(&verdict), None)
            .await?;
        Err(MigrationError::invalid(message))
    }
    pub async fn gate(&self, id: &str, options: &Value, job: &str) -> MResult<Value> {
        let m = self
            .store
            .transition(
                id,
                &["verified", "gate_failed", "gating"],
                "gating",
                None,
                None,
            )
            .await?;
        let pinned_a = &m["totals"]["verified_authority"];
        let pinned_s = &m["totals"]["verified_target_scope"];
        if !pinned_a.is_object() || !pinned_s.is_object() {
            return self.fail_gate(id, json!({"passed":false,"error":"gate requires pinned verify authority evidence","phase":"precheck"}), "gate requires pinned verify authority evidence; abort and restart the migration").await;
        }
        let mut pre_authority = Value::Null;
        let mut pre_scope = Value::Null;
        let precheck: MResult<()> = async {
            let (_, d, _, target) = self.context(id).await?;
            let pending = self.store.pending(id, &d.dataset_id).await?;
            let authority = self.store.authority(&d.dataset_id).await?;
            let scope = self.scan_target(&d, &target).await?;
            pre_authority = authority;
            pre_scope = scope;
            if pending != 0 {
                return Err(MigrationError::invalid(format!("{pending} enabled chunks are not yet embedded into the shadow collection")));
            }
            if !matches(&pre_authority,&pre_scope,SCOPE) {
                return Err(MigrationError::invalid("shadow collection does not match the PostgreSQL authority (point/source digest mismatch)"));
            }
            Ok(())
        }.await;
        if let Err(error) = precheck {
            return self.fail_gate(id, json!({"passed":false,"error":error.message,"phase":"precheck","authority_snapshot":pre_authority,"target_scope":pre_scope}), "evaluation gate authority precheck failed; abort and restart the migration").await;
        }
        if !matches(&pre_authority, pinned_a, AUTHORITY) || !matches(&pre_scope, pinned_s, SCOPE) {
            let error = if !matches(&pre_authority, pinned_a, AUTHORITY) {
                "PostgreSQL corpus changed after verification"
            } else {
                "shadow collection changed after verification"
            };
            return self.fail_gate(id, json!({"passed":false,"error":error,"phase":"precheck","authority_snapshot":pre_authority,"target_scope":pre_scope}), "evaluation gate authority precheck failed; abort and restart the migration").await;
        }
        let mut verdict = match self.evaluate(id, options).await {
            Ok(verdict) => verdict,
            Err(error) => return self.fail_gate(id, json!({"passed":false,"error":error.message,"phase":"evaluation","authority_snapshot":pre_authority,"target_scope":pre_scope}), "evaluation gate crashed").await,
        };
        verdict["action_job_id"] = json!(job);
        let after = self.evidence(id).await.and_then(|(a, s)| {
            if !matches(&a, pinned_a, AUTHORITY) {
                Err(MigrationError::invalid(
                    "PostgreSQL corpus changed after verification",
                ))
            } else if !matches(&s, pinned_s, SCOPE) {
                Err(MigrationError::invalid(
                    "shadow collection changed after verification",
                ))
            } else {
                Ok((a, s))
            }
        });
        match after {
            Ok((authority, scope)) => {
                verdict["authority_snapshot"] = authority;
                verdict["target_scope"] = scope;
            }
            Err(error) => {
                verdict["passed"] = json!(false);
                verdict["error"] =
                    json!(format!("authority changed during gate: {}", error.message));
                verdict["phase"] = json!("postcheck");
                verdict["authority_snapshot"] = pre_authority;
                verdict["target_scope"] = pre_scope;
                return self
                    .fail_gate(
                        id,
                        verdict,
                        "evaluation gate authority changed; abort and restart the migration",
                    )
                    .await;
            }
        }
        let passed = verdict["passed"] == true;
        let m = self
            .store
            .transition(
                id,
                &["gating"],
                if passed { "ready" } else { "gate_failed" },
                Some(&verdict),
                None,
            )
            .await?;
        Ok(json!({"migration":m,"verdict":verdict,"passed":passed}))
    }
    pub async fn evaluate(&self, id: &str, options: &Value) -> MResult<Value> {
        let size = options["sample_size"].as_u64().unwrap_or(16).clamp(1, 256);
        let k = options["top_k"].as_u64().unwrap_or(5).clamp(1, 50) as usize;
        let tolerance = options["tolerance"].as_f64().unwrap_or(0.10);
        let floor = options["floor"].as_f64().unwrap_or(0.80);
        let mut verdict = json!({"evaluator":"shadow_serving_self_retrieval","passed":false,"samples":0,"top_k":k,"tolerance":tolerance,"floor":floor,"shadow_hit_rate":null,"serving_hit_rate":null});
        let measured: MResult<()> = async {
            let (_, d, source, target) = self.context(id).await?;
            if d.dataset_id.trim().is_empty() || text(&source,"collection_name").trim().is_empty() || text(&target,"collection_name").trim().is_empty() {
                verdict["reason"] = json!("gate context is missing dataset/serving/target collection bindings; cannot measure the candidate generation");
                return Ok(());
            }
            let rows:Vec<(String,String)>=sqlx::query_as(&format!("SELECT s.segment_id,s.text{ENABLED} AND COALESCE(s.text,'')<>'' ORDER BY random() LIMIT $2")).bind(&d.dataset_id).bind(size as i64).fetch_all(&self.store.pool).await?;
            let rows: Vec<_> = rows.into_iter().filter(|(id,text)| !id.trim().is_empty() && !text.trim().is_empty()).collect();
            verdict["samples"] = json!(rows.len());
            if rows.is_empty() {
                verdict["reason"] = json!("no enabled text segments available to probe; a migration that cannot be measured must not cut over");
                return Ok(());
            }
            let texts: Vec<_> = rows.iter().map(|r| r.1.clone()).collect();
            for (b, label, rate_key) in [(&target,"target","shadow_hit_rate"),(&source,"serving","serving_hit_rate")] {
                let bd = binding_dataset(&d, b);
                let vectors = match self.embeddings.embed_documents(&bd, &texts).await {
                    Ok(v) => v,
                    Err(error) => {
                        verdict["reason"] = json!(format!("{label} generation cannot embed the probe corpus: {error}"));
                        return Ok(());
                    }
                };
                if vectors.len() != rows.len() || vectors.iter().any(Vec::is_empty) {
                    verdict["reason"] = json!(format!("{label} embedder returned {} vectors for {} probes", vectors.len(), rows.len()));
                    return Ok(());
                }
                let mut hits = 0;
                for ((segment, _), vector) in rows.iter().zip(vectors) {
                    let found = self.vectors.search(&bd, &vector, k, &RetrieveRequest::default()).await?;
                    if found.iter().any(|h| {
                        let payload_id = text(&h.payload,"segment_id");
                        let identity = if payload_id.is_empty() {h.point_id.as_str()} else {payload_id};
                        identity.trim() == segment.trim()
                    }) { hits += 1; }
                }
                verdict[rate_key] = json!(((hits as f64 / rows.len() as f64) * 1e6).round_ties_even() / 1e6);
            }
            // The original evaluator compares the published, rounded rates.
            let shadow = verdict["shadow_hit_rate"].as_f64().unwrap();
            let serving = verdict["serving_hit_rate"].as_f64().unwrap();
            let passed = shadow >= floor && shadow >= serving - tolerance;
            verdict["passed"] = json!(passed);
            if !passed {
                verdict["reason"] = json!(if shadow < floor {
                    format!("shadow self-retrieval hit-rate {shadow:.3} is below the required floor {floor:.3}")
                } else {
                    format!("shadow self-retrieval hit-rate {shadow:.3} regressed against serving {serving:.3} beyond tolerance {tolerance:.3}")
                });
            }
            Ok(())
        }.await;
        if let Err(error) = measured {
            verdict["passed"] = json!(false);
            verdict["reason"] = json!(format!("gate measurement failed: {}", error.message));
        }
        Ok(verdict)
    }
    pub async fn cutover(&self, id: &str, retention: i64) -> MResult<Value> {
        if retention < 0 {
            return Err(MigrationError::invalid(
                "retention_seconds cannot be negative",
            ));
        }
        let (_, d, _, target) = self.context(id).await?;
        let mut lease = SessionLease::acquire(
            &self.store.pool,
            format!("knowledge-dataset-index:{}", d.dataset_id),
        )
        .await?;
        let scope = self.scan_target(&d, &target).await?;
        let mut tx = lease.connection().begin().await?;
        let m = locked_migration(&mut tx, id).await?;
        if m["state"] != "ready" || m["gate"]["passed"] != true {
            return Err(MigrationError::conflict(
                "cutover requires ready migration and passing pinned gate",
            ));
        }
        let authority = MigrationStore::authority_on(&mut tx, &d.dataset_id).await?;
        if MigrationStore::pending_on(&mut tx, id, &d.dataset_id).await? != 0
            || !matches(&authority, &m["gate"]["authority_snapshot"], AUTHORITY)
            || !matches(&scope, &m["gate"]["target_scope"], SCOPE)
            || !matches(&authority, &scope, SCOPE)
        {
            return Err(MigrationError::conflict(
                "corpus or shadow collection changed after gate",
            ));
        }
        let source = locked_binding(&mut tx, text(&m, "source_binding_id")).await?;
        let target = locked_binding(&mut tx, text(&m, "target_binding_id")).await?;
        if source["state"] != "serving" || target["state"] != "shadow" {
            return Err(MigrationError::conflict(
                "serving or shadow binding changed",
            ));
        }
        flip_dataset(
            &mut tx,
            &d.dataset_id,
            &source,
            &target,
            authority["content_revision"].as_i64(),
        )
        .await?;
        sqlx::query("UPDATE dataset_collection_bindings SET state='retained',retired_at=NOW(),retained_until=NOW()+make_interval(secs=>$2) WHERE binding_id=$1::uuid AND state='serving'").bind(text(&source,"binding_id")).bind(retention as f64).execute(&mut *tx).await?;
        sqlx::query("UPDATE dataset_collection_bindings SET state='serving',activated_at=NOW() WHERE binding_id=$1::uuid AND state='shadow'").bind(text(&target,"binding_id")).execute(&mut *tx).await?;
        stamp_documents(&mut tx, &d.dataset_id, &target).await?;
        let m:Value=sqlx::query_scalar("UPDATE embedding_migrations SET state='completed',error=NULL,updated_at=NOW() WHERE migration_id=$1::uuid AND state='ready' RETURNING to_jsonb(embedding_migrations)").bind(id).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        lease.release().await?;
        Ok(json!({"migration":m}))
    }
    pub async fn rollback(&self, id: &str, keep_shadow: bool) -> MResult<Value> {
        let mut tx = self.store.pool.begin().await?;
        let m = locked_migration(&mut tx, id).await?;
        if m["state"] != "completed" {
            return Err(MigrationError::conflict(
                "only completed migrations can roll back",
            ));
        }
        let dataset = text(&m, "dataset_id");
        dataset_lock(&mut tx, dataset).await?;
        let source = locked_binding(&mut tx, text(&m, "source_binding_id")).await?;
        let target = locked_binding(&mut tx, text(&m, "target_binding_id")).await?;
        if source["state"] != "retained" || target["state"] != "serving" {
            return Err(MigrationError::conflict(
                "rollback source was retired or newer generation is serving",
            ));
        }
        flip_dataset(&mut tx, dataset, &target, &source, None).await?;
        sqlx::query("UPDATE dataset_collection_bindings SET state=$2,retired_at=CASE WHEN $2='retired' THEN NOW() ELSE retired_at END WHERE binding_id=$1::uuid AND state='serving'").bind(text(&target,"binding_id")).bind(if keep_shadow{"shadow"}else{"retired"}).execute(&mut *tx).await?;
        sqlx::query("UPDATE dataset_collection_bindings SET state='serving',activated_at=NOW(),retained_until=NULL WHERE binding_id=$1::uuid AND state='retained'").bind(text(&source,"binding_id")).execute(&mut *tx).await?;
        stamp_documents(&mut tx, dataset, &source).await?;
        let m:Value=sqlx::query_scalar("UPDATE embedding_migrations SET state='rolled_back',updated_at=NOW() WHERE migration_id=$1::uuid RETURNING to_jsonb(embedding_migrations)").bind(id).fetch_one(&mut *tx).await?;
        if !keep_shadow {
            sqlx::query("DELETE FROM embedding_migration_progress WHERE migration_id=$1::uuid")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(json!({"migration":m}))
    }
    pub async fn abort(&self, id: &str, reason: &str, purge: bool) -> MResult<Value> {
        let (_, d, _, target) = self.context(id).await?;
        let mut tx = self.store.pool.begin().await?;
        dataset_lock(&mut tx, &d.dataset_id).await?;
        let m = locked_migration(&mut tx, id).await?;
        if ![
            "shadow_build",
            "backfilling",
            "verified",
            "gating",
            "ready",
            "failed",
            "gate_failed",
            "rolled_back",
        ]
        .contains(&text(&m, "state"))
        {
            return Err(MigrationError::conflict("migration cannot be aborted"));
        }
        let exists: Option<String> = sqlx::query_scalar(
            "SELECT to_regclass('knowledge.embedding_migration_action_jobs')::text",
        )
        .fetch_one(&mut *tx)
        .await?;
        if exists.is_some() {
            let running:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM knowledge.embedding_migration_action_jobs WHERE migration_id=$1::uuid AND state='running')").bind(id).fetch_one(&mut *tx).await?;
            if running {
                return Err(MigrationError::conflict(
                    "running migration action must finish or be cancelled first",
                ));
            }
            sqlx::query("UPDATE knowledge.embedding_migration_action_jobs SET state='failed',error='cancelled by abort',finished_at=NOW(),updated_at=NOW() WHERE migration_id=$1::uuid AND state='queued'").bind(id).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE dataset_collection_bindings SET state='retired',retired_at=NOW() WHERE binding_id=$1::uuid AND state='shadow'").bind(text(&target,"binding_id")).execute(&mut *tx).await?;
        let m:Value=sqlx::query_scalar("UPDATE embedding_migrations SET state='abandoned',error=$2,updated_at=NOW() WHERE migration_id=$1::uuid RETURNING to_jsonb(embedding_migrations)").bind(id).bind(reason.chars().take(2000).collect::<String>()).fetch_one(&mut *tx).await?;
        sqlx::query("DELETE FROM embedding_migration_progress WHERE migration_id=$1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        if purge {
            let current = self.store.binding(text(&target, "binding_id")).await?;
            if current["state"] == "retired"
                && self
                    .vectors
                    .collection_exists(text(&target, "collection_name"))
                    .await?
            {
                self.vectors
                    .delete_collection(&binding_dataset(&d, &target))
                    .await?;
            }
        }
        Ok(json!({"migration":m}))
    }
    pub async fn reclaim_expired(&self, delete_collections: bool) -> MResult<Vec<String>> {
        let candidates:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(b) FROM dataset_collection_bindings b WHERE state='retained' AND retained_until<=NOW()").fetch_all(&self.store.pool).await?;
        let mut reclaimed = Vec::new();
        for b in candidates {
            let mut tx = self.store.pool.begin().await?;
            dataset_lock(&mut tx, text(&b, "dataset_id")).await?;
            let row:Option<Value>=sqlx::query_scalar("UPDATE dataset_collection_bindings SET state='retired',retired_at=NOW() WHERE binding_id=$1::uuid AND state='retained' AND retained_until<=NOW() RETURNING to_jsonb(dataset_collection_bindings)").bind(text(&b,"binding_id")).fetch_optional(&mut *tx).await?;
            tx.commit().await?;
            if row.is_none() {
                continue;
            }
            if delete_collections {
                let base = Dataset {
                    dataset_id: text(&b, "dataset_id").into(),
                    tenant_id: text(&b, "tenant_id").into(),
                    ..Default::default()
                };
                let d = binding_dataset(&base, &b);
                if self.vectors.collection_exists(d.collection()?).await? {
                    self.vectors.delete_collection(&d).await?;
                }
            }
            reclaimed.push(text(&b, "binding_id").into());
        }
        Ok(reclaimed)
    }
    pub async fn describe(&self, d: &Dataset) -> MResult<Value> {
        let serving = match self.store.serving(d).await {
            Ok(binding) => Some(binding),
            Err(error) if error.status == 409 || error.status == 400 => None,
            Err(error) => return Err(error),
        };
        // A recoverable attempt can be older than the bounded history window.
        let live: Option<Value> = sqlx::query_scalar("SELECT to_jsonb(m) FROM embedding_migrations m WHERE dataset_id=$1 AND (state=ANY($2::text[]) OR state IN ('failed','gate_failed')) ORDER BY CASE WHEN state=ANY($2::text[]) THEN 0 ELSE 1 END,created_at DESC,migration_id DESC LIMIT 1")
            .bind(&d.dataset_id).bind(LIVE).fetch_optional(&self.store.pool).await?;
        let recent:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(m) FROM embedding_migrations m WHERE dataset_id=$1 ORDER BY created_at DESC,migration_id DESC LIMIT 5").bind(&d.dataset_id).fetch_all(&self.store.pool).await?;
        let selected = live.as_ref().or(recent.first());
        let enabled: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*){ENABLED}"))
            .bind(&d.dataset_id)
            .fetch_one(&self.store.pool)
            .await?;
        let mut out = json!({"dataset_id":d.dataset_id,"serving_binding":serving,"live_migration":live,"latest_migration":recent.first(),"recent_migrations":recent,"source_binding":null,"target_binding":null,"pending_chunks":null,"enabled_chunks":enabled});
        let totals = selected.map(|m| &m["totals"]).filter(|t| t.is_object());
        let gate_report = selected.map(|m| &m["gate"]).filter(|g| g.is_object());
        let mut health = json!({"status":"unknown","checked_live":false,"collection_name":null,"pending_chunks":null,"authority":null,"target_scope":null,"verified_authority":totals.and_then(|t| t.get("verified_authority")),"verified_target_scope":totals.and_then(|t| t.get("verified_target_scope")),"gate_report":gate_report,"reason":"no migration target is available"});
        if let Some(m) = selected {
            out["source_binding"] = self
                .store
                .optional_binding(text(m, "source_binding_id"))
                .await?
                .unwrap_or(Value::Null);
            let target = self
                .store
                .optional_binding(text(m, "target_binding_id"))
                .await?;
            let pending = self
                .store
                .pending(text(m, "migration_id"), &d.dataset_id)
                .await?;
            out["pending_chunks"] = json!(pending);
            health["pending_chunks"] = json!(pending);
            if let Some(target) = target {
                health["collection_name"] = target["collection_name"].clone();
                if target["state"] == "retired" {
                    health["reason"] = json!("migration target binding is retired");
                } else {
                    let evidence: MResult<(Value, Value)> = async {
                        let authority = self.store.authority(&d.dataset_id).await?;
                        let scope = self.scan_target(d, &target).await?;
                        Ok((authority, scope))
                    }
                    .await;
                    match evidence {
                        Ok((authority, scope)) => {
                            let healthy = pending == 0 && matches(&authority, &scope, SCOPE);
                            health["status"] = json!(if healthy { "healthy" } else { "drifted" });
                            health["checked_live"] = json!(true);
                            health["authority"] = authority;
                            health["target_scope"] = scope;
                            health["reason"] = if healthy {
                                Value::Null
                            } else {
                                json!("pending chunks or point/source digest mismatch")
                            };
                        }
                        Err(_) => {
                            health["reason"] = json!("live collection health check is unavailable")
                        }
                    }
                }
                out["target_binding"] = target;
            }
        }
        out["collection_health"] = health;
        let jobs:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(j) FROM knowledge.embedding_migration_action_jobs j WHERE dataset_id=$1 ORDER BY CASE WHEN state IN ('queued','running') THEN 0 ELSE 1 END,COALESCE(finished_at,updated_at) DESC,created_at DESC,job_id DESC LIMIT 11").bind(&d.dataset_id).fetch_all(&self.store.pool).await?;
        out["active_action_job"] = jobs
            .iter()
            .find(|j| ["queued", "running"].contains(&text(j, "state")))
            .map(super::migration_jobs::public_job)
            .unwrap_or(Value::Null);
        out["recent_action_jobs"] = json!(
            jobs.iter()
                .filter(|j| ["succeeded", "failed"].contains(&text(j, "state")))
                .take(10)
                .map(super::migration_jobs::public_job)
                .collect::<Vec<_>>()
        );
        Ok(out)
    }
}
async fn locked_migration(c: &mut PgConnection, id: &str) -> MResult<Value> {
    let m: Option<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(m) FROM embedding_migrations m WHERE migration_id=$1::uuid FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(c)
    .await?;
    m.ok_or_else(|| MigrationError {
        status: 404,
        message: "migration not found".into(),
    })
}
async fn locked_binding(c: &mut PgConnection, id: &str) -> MResult<Value> {
    let b:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(b) FROM dataset_collection_bindings b WHERE binding_id=$1::uuid FOR UPDATE").bind(id).fetch_optional(c).await?;
    b.ok_or_else(|| MigrationError::conflict("binding missing"))
}
async fn flip_dataset(
    c: &mut PgConnection,
    dataset: &str,
    from: &Value,
    to: &Value,
    revision: Option<i64>,
) -> MResult<()> {
    let changed=sqlx::query("UPDATE datasets SET collection_name=$3,embedding_provider=$4,embedding_model=$5,embedding_model_version=$6,embedding_dimension=$7,updated_at=NOW() WHERE dataset_id=$1 AND is_deleted=FALSE AND COALESCE(collection_name,'')=$2 AND ($8::bigint IS NULL OR content_revision=$8)").bind(dataset).bind(text(from,"collection_name")).bind(text(to,"collection_name")).bind(text(to,"embedding_provider")).bind(text(to,"embedding_model")).bind(text(to,"embedding_model_version")).bind(to["embedding_dimension"].as_i64().unwrap_or(0)as i32).bind(revision).execute(c).await?.rows_affected();
    if changed != 1 {
        return Err(MigrationError::conflict(
            "dataset serving generation changed",
        ));
    }
    Ok(())
}
async fn stamp_documents(c: &mut PgConnection, dataset: &str, b: &Value) -> MResult<()> {
    sqlx::query("UPDATE documents SET embedding_model=$2,embedding_model_version=$3,embedding_dimension=$4 WHERE dataset_id=$1").bind(dataset).bind(text(b,"embedding_model")).bind(text(b,"embedding_model_version")).bind(b["embedding_dimension"].as_i64().unwrap_or(0)as i32).execute(c).await?;
    Ok(())
}
impl SessionLease {
    pub async fn acquire_shared(pool: &PgPool, name: String) -> MResult<Self> {
        let mut c = pool.acquire().await?;
        let held: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_lock_shared(hashtextextended($1,0))")
                .bind(&name)
                .fetch_one(&mut *c)
                .await?;
        if !held {
            return Err(MigrationError::conflict(
                "dataset index lifecycle barrier is held",
            ));
        }
        Ok(Self {
            connection: Some(c),
            name,
            shared: true,
        })
    }
}
