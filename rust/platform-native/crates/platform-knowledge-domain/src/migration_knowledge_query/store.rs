use super::{
    config::RetrieveRequest,
    types::{Chunk, Dataset, IndexGeneration, QueryError, Result, UserContext},
};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, QueryBuilder};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
pub struct QueryStore {
    pub pool: PgPool,
}
const ACTIVE: &str = " FROM segments s JOIN documents d ON d.document_id=s.document_id AND d.dataset_id=s.dataset_id JOIN datasets ds ON ds.dataset_id=s.dataset_id WHERE ds.is_deleted=FALSE AND COALESCE(s.enabled,TRUE)=TRUE AND s.status='completed' AND COALESCE(d.enabled,TRUE)=TRUE AND COALESCE(d.archived,FALSE)=FALSE AND NOT(COALESCE(d.metadata,'{}'::jsonb) ? '_document_lifecycle_reindex') ";
impl QueryStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub async fn dataset(&self, id: &str) -> Result<Dataset> {
        let value: Option<Value> = sqlx::query_scalar(
            "SELECT to_jsonb(d) FROM datasets d WHERE dataset_id=$1 AND is_deleted=FALSE",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        serde_json::from_value(value.ok_or(QueryError::NotFound)?).map_err(|_| QueryError::Database)
    }
    pub async fn require_dataset_access(
        &self,
        user: &UserContext,
        id: &str,
        required: &str,
    ) -> Result<Dataset> {
        let d = self.dataset(id).await?;
        if d.extra.get("is_archived").and_then(Value::as_bool) == Some(true) {
            return Err(QueryError::Forbidden);
        }
        let visibility = d.visibility.to_lowercase();
        let same = !d.tenant_id.trim().is_empty()
            && !user.tenant_id.trim().is_empty()
            && d.tenant_id.trim() == user.tenant_id.trim();
        let permission = if user.anonymous() {
            if visibility == "public" { 1 } else { 0 }
        } else if (same && (user.user_tier == "admin" || user.roles.iter().any(|r| r == "admin")))
            || d.created_by
                .as_deref()
                .is_some_and(|id| !id.is_empty() && id == user.user_id)
        {
            3
        } else {
            let roles = if same { user.roles.clone() } else { vec![] };
            let permissions:Vec<String>=sqlx::query_scalar("SELECT permission FROM dataset_permissions WHERE dataset_id=$1 AND ((subject_type='user' AND subject_id=$2) OR (subject_type='tenant_role' AND subject_id=ANY($3::text[])))").bind(id).bind(&user.user_id).bind(roles).fetch_all(&self.pool).await?;
            let explicit = permissions
                .iter()
                .map(|p| permission_rank(p))
                .max()
                .unwrap_or(0);
            if explicit > 0 {
                explicit
            } else if visibility == "public" || (visibility == "tenant" && same) {
                1
            } else {
                0
            }
        };
        if permission < permission_rank(required) {
            return Err(QueryError::Forbidden);
        }
        Ok(d)
    }
    pub async fn require_generation(
        &self,
        user: &UserContext,
        id: &str,
        expected: &IndexGeneration,
    ) -> Result<()> {
        let dataset = self.require_dataset_access(user, id, "viewer").await?;
        let current = dataset.generation().map_err(|e| {
            if e == QueryError::PublicationInProgress {
                QueryError::GenerationChanged
            } else {
                e
            }
        })?;
        if &current != expected {
            Err(QueryError::GenerationChanged)
        } else {
            Ok(())
        }
    }
    pub async fn active_segment_ids(
        &self,
        dataset: &Dataset,
        ids: &[String],
    ) -> Result<BTreeSet<String>> {
        if ids.is_empty() {
            return Ok(BTreeSet::new());
        }
        require_scope(dataset)?;
        let query = format!(
            "SELECT DISTINCT s.segment_id{ACTIVE}AND s.dataset_id=$1 AND ds.tenant_id=$2 AND s.segment_id=ANY($3::text[])"
        );
        let rows: Vec<String> = sqlx::query_scalar(&query)
            .bind(&dataset.dataset_id)
            .bind(&dataset.tenant_id)
            .bind(ids)
            .fetch_all(&self.pool)
            .await?;
        if rows.iter().any(|id| !ids.contains(id)) {
            return Err(QueryError::Database);
        }
        Ok(rows.into_iter().collect())
    }
    pub async fn segment_scoped(&self, dataset: &Dataset, id: &str) -> Result<Option<Chunk>> {
        require_scope(dataset)?;
        let query = format!(
            "SELECT to_jsonb(s){ACTIVE}AND s.dataset_id=$1 AND ds.tenant_id=$2 AND s.segment_id=$3"
        );
        let value: Option<Value> = sqlx::query_scalar(&query)
            .bind(&dataset.dataset_id)
            .bind(&dataset.tenant_id)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        value
            .map(|v| serde_json::from_value(v).map_err(|_| QueryError::Database))
            .transpose()
    }
    pub async fn search_segments_text(
        &self,
        dataset: &Dataset,
        terms: &[String],
        request: &RetrieveRequest,
        limit: usize,
    ) -> Result<Vec<Chunk>> {
        if terms.is_empty() {
            return Ok(vec![]);
        }
        match self.search_fts(dataset, terms, request, limit).await {
            Ok(rows) => Ok(rows),
            Err(e) => {
                if let sqlx::Error::Database(db) = &e {
                    if db.code().as_deref() != Some("42703") {
                        return Err(QueryError::Database);
                    }
                } else {
                    return Err(QueryError::Database);
                }
                self.search_ilike(dataset, terms, request, limit).await
            }
        }
    }
    async fn search_fts(
        &self,
        d: &Dataset,
        terms: &[String],
        r: &RetrieveRequest,
        limit: usize,
    ) -> std::result::Result<Vec<Chunk>, sqlx::Error> {
        let mut q = QueryBuilder::<Postgres>::new("SELECT to_jsonb(s)");
        q.push(ACTIVE);
        append_filters(&mut q, d, r);
        // Source joins tsqueries with SQL || (OR), despite its stale AND comment.
        q.push(" AND s.text_search @@ (");
        for (i, t) in terms.iter().enumerate() {
            if i > 0 {
                q.push(" || ");
            }
            q.push("plainto_tsquery('simple',")
                .push_bind(t.clone())
                .push(")");
        }
        q.push(") ORDER BY ts_rank_cd(s.text_search,(");
        for (i, t) in terms.iter().enumerate() {
            if i > 0 {
                q.push(" || ");
            }
            q.push("plainto_tsquery('simple',")
                .push_bind(t.clone())
                .push(")");
        }
        q.push(")) DESC LIMIT ").push_bind(limit as i64);
        let rows: Vec<Value> = q.build_query_scalar().fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|v| serde_json::from_value(v).map_err(|e| sqlx::Error::Decode(Box::new(e))))
            .collect()
    }
    pub async fn search_ilike(
        &self,
        d: &Dataset,
        terms: &[String],
        r: &RetrieveRequest,
        limit: usize,
    ) -> Result<Vec<Chunk>> {
        require_scope(d)?;
        let mut q = QueryBuilder::<Postgres>::new("SELECT to_jsonb(s)");
        q.push(ACTIVE);
        append_filters(&mut q, d, r);
        if !terms.is_empty() {
            q.push(" AND (");
            for (i, t) in terms.iter().enumerate() {
                if i > 0 {
                    q.push(" OR ");
                }
                let escaped = t
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_");
                q.push("s.text ILIKE ").push_bind(format!("%{escaped}%"));
            }
            q.push(")");
        }
        q.push(" LIMIT ").push_bind(limit as i64);
        let rows: Vec<Value> = q.build_query_scalar().fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|v| serde_json::from_value(v).map_err(|_| QueryError::Database))
            .collect()
    }
    pub async fn published_source_identities(
        &self,
        d: &Dataset,
        ids: &[String],
    ) -> Result<BTreeMap<String, (i64, String)>> {
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        require_scope(d)?;
        let rows:Vec<(String,i32,String)>=sqlx::query_as("SELECT d.document_id,d.current_version,lower(v.content_hash) AS source_hash FROM documents d JOIN datasets ds ON ds.dataset_id=d.dataset_id JOIN document_versions v ON v.document_id=d.document_id AND v.version_number=d.current_version WHERE d.dataset_id=$1 AND ds.tenant_id=$2 AND ds.is_deleted=FALSE AND d.document_id=ANY($3::text[]) AND COALESCE(d.enabled,TRUE)=TRUE AND COALESCE(d.archived,FALSE)=FALSE AND NOT(COALESCE(d.metadata,'{}'::jsonb) ? '_document_lifecycle_reindex') AND v.change_type NOT IN ('pending_restore','pending_before_restore') AND v.content=d.content AND encode(sha256(convert_to(v.content,'UTF8')),'hex')=lower(v.content_hash)").bind(&d.dataset_id).bind(&d.tenant_id).bind(ids).fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|(id, n, h)| (id, (n as i64, h)))
            .collect())
    }
    /// Source keeps version-store outages nonfatal; only a known identity mismatch fails.
    pub async fn serving_binding(&self, dataset_id: &str) -> Option<Value> {
        sqlx::query_scalar("SELECT to_jsonb(b) FROM dataset_collection_bindings b WHERE dataset_id=$1 AND state='serving'").bind(dataset_id).fetch_optional(&self.pool).await.ok().flatten()
    }
    pub async fn record_query(
        &self,
        d: &Dataset,
        user: &UserContext,
        query: &str,
        mode: &str,
        source: &str,
        top_k: usize,
        results: &[super::types::RetrieveResult],
        metadata: &Value,
    ) -> Result<()> {
        let trace = metadata["trace_id"].as_str().unwrap_or("");
        let fingerprint = metadata["query_fingerprint"].as_str().unwrap_or("");
        if trace.is_empty() || fingerprint.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = results
            .iter()
            .map(|r| r.segment_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let normalized = query.split_whitespace().collect::<Vec<_>>().join(" ");
        let meta = json!({"query_fingerprint":fingerprint,"mode":mode,"top_k":top_k,"hit_count":results.len(),"cache_hit":metadata["retrieval_cache_hit"],"trace_id":trace,"stage_timings":metadata["timings_ms"],"rerank_degraded":metadata["rerank_degraded"]});
        let mut tx = self.pool.begin().await?;
        let inserted:Option<i32>=sqlx::query_scalar("INSERT INTO dataset_queries(dataset_id,content,source,created_by_role,created_by,metadata,trace_id,query_fingerprint,mode,top_k,hit_count,stage_timings) VALUES($1,$2,$12,$3,$4,$5::jsonb,$6::uuid,$7,$8,$9,$10,$11::jsonb) ON CONFLICT(trace_id) WHERE trace_id IS NOT NULL DO NOTHING RETURNING 1").bind(&d.dataset_id).bind(normalized).bind(&user.user_tier).bind(&user.user_id).bind(meta).bind(trace).bind(fingerprint).bind(mode).bind(top_k as i32).bind(results.len()as i32).bind(metadata.get("timings_ms").cloned().unwrap_or_else(||json!({}))).bind(source).fetch_optional(&mut *tx).await?;
        if inserted.is_some() && !ids.is_empty() {
            sqlx::query("UPDATE segments SET hit_count=hit_count+1 WHERE dataset_id=$1 AND segment_id=ANY($2::text[])").bind(&d.dataset_id).bind(ids).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn embedding_cache(
        &self,
        hashes: &[String],
        identity: &Value,
    ) -> Result<BTreeMap<String, Vec<f64>>> {
        let rows:Vec<(String,Vec<f64>)>=sqlx::query_as("SELECT content_hash,vector FROM embedding_vector_cache WHERE embedding_provider=$1 AND embedding_model=$2 AND embedding_model_version=$3 AND content_hash=ANY($4::text[])").bind(identity["embedding_provider"].as_str().unwrap_or("")).bind(identity["embedding_model"].as_str().unwrap_or("")).bind(identity["embedding_model_version"].as_str().unwrap_or("")).bind(hashes).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().collect())
    }
}
fn require_scope(d: &Dataset) -> Result<()> {
    if d.tenant_id.trim().is_empty() || d.dataset_id.trim().is_empty() {
        Err(QueryError::Validation(
            "dataset and tenant scope are required".into(),
        ))
    } else {
        Ok(())
    }
}
fn permission_rank(s: &str) -> i32 {
    match s {
        "viewer" => 1,
        "editor" => 2,
        "owner" => 3,
        _ => 0,
    }
}
fn append_filters<'a>(q: &mut QueryBuilder<'a, Postgres>, d: &Dataset, r: &RetrieveRequest) {
    q.push(" AND s.dataset_id=")
        .push_bind(d.dataset_id.clone())
        .push(" AND ds.tenant_id=")
        .push_bind(d.tenant_id.clone());
    for (field, value) in [
        ("document_id", &r.document_id),
        ("source_type", &r.source_type_filter),
        ("language", &r.language_filter),
    ] {
        if let Some(v) = value {
            q.push(format!(" AND s.{field}=")).push_bind(v.clone());
        }
    }
    if let Some(value) = &r.metadata_filter {
        q.push(" AND s.metadata @> ")
            .push_bind(value.clone())
            .push("::jsonb");
    }
}
