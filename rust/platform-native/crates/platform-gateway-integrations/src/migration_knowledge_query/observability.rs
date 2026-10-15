//! Tenant-bound query keyset pagination and idempotent per-actor feedback writes.
use super::{
    store::QueryStore,
    types::{QueryError, Result, UserContext},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, QueryBuilder};
fn invalid(message: &str) -> QueryError {
    QueryError::Validation(message.into())
}
fn fifty() -> usize {
    50
}
#[derive(Deserialize)]
pub struct QueryHistoryFilter {
    #[serde(default = "fifty")]
    pub limit: usize,
    pub zero_results: Option<bool>,
    pub mode: Option<String>,
    pub cursor: Option<String>,
}
#[derive(Deserialize)]
pub struct FeedbackFilter {
    #[serde(default = "fifty")]
    pub limit: usize,
    pub rating: Option<String>,
    pub reason_code: Option<String>,
    pub target_type: Option<String>,
    pub trace_id: Option<uuid::Uuid>,
    pub cursor: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackUpsert {
    pub trace_id: uuid::Uuid,
    pub query_fingerprint: String,
    pub target_type: String,
    pub segment_id: Option<String>,
    pub rating: String,
    pub reason_code: String,
    pub comment: Option<String>,
}
pub enum ObservationError {
    Conflict,
    Query(QueryError),
}
impl From<QueryError> for ObservationError {
    fn from(e: QueryError) -> Self {
        Self::Query(e)
    }
}
impl From<sqlx::Error> for ObservationError {
    fn from(_: sqlx::Error) -> Self {
        Self::Query(QueryError::Database)
    }
}
impl FeedbackUpsert {
    fn validate(&self) -> Result<()> {
        if self.query_fingerprint.len() != 64
            || !self
                .query_fingerprint
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !["retrieval_hit", "qa_answer"].contains(&self.target_type.as_str())
            || !["positive", "negative"].contains(&self.rating.as_str())
            || self
                .comment
                .as_ref()
                .is_some_and(|s| s.chars().count() > 2000)
            || self
                .segment_id
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.chars().count() > 255)
        {
            return Err(invalid("invalid query feedback"));
        }
        if (self.target_type == "retrieval_hit" && self.segment_id.is_none())
            || (self.target_type == "qa_answer" && self.segment_id.is_some())
        {
            return Err(invalid(
                "feedback segment identity is incompatible with target_type",
            ));
        }
        let allowed: &[&str] = if self.rating == "positive" {
            &["relevant", "helpful", "well_cited", "other"]
        } else {
            &[
                "irrelevant",
                "incorrect",
                "missing_context",
                "bad_citation",
                "stale",
                "unsafe",
                "other",
            ]
        };
        if !allowed.contains(&self.reason_code.as_str()) {
            return Err(invalid("reason_code is incompatible with rating"));
        }
        Ok(())
    }
}
pub fn decode_query_cursor(cursor: &str) -> Result<(DateTime<Utc>, String)> {
    let raw = cursor.trim().trim_end_matches('=');
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| invalid("invalid query pagination cursor"))?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| invalid("invalid query pagination cursor"))?;
    let date = DateTime::parse_from_rfc3339(value["created_at"].as_str().unwrap_or(""))
        .map_err(|_| invalid("invalid query pagination cursor"))?
        .with_timezone(&Utc);
    let id = value["id"].as_str().unwrap_or("").trim().to_owned();
    if id.is_empty() {
        return Err(invalid("invalid query pagination cursor"));
    }
    Ok((date, id))
}
pub fn encode_query_cursor(created: &str, id: &str) -> Result<String> {
    let date = DateTime::parse_from_rfc3339(created).map_err(|_| QueryError::Database)?;
    let date = date.to_rfc3339_opts(
        if date.timestamp_subsec_micros() == 0 {
            chrono::SecondsFormat::Secs
        } else {
            chrono::SecondsFormat::Micros
        },
        false,
    );
    Ok(URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({"created_at":date,"id":id}))
            .map_err(|_| QueryError::Database)?,
    ))
}
fn page(mut rows: Vec<Value>, limit: usize, id: &str, key: &str) -> Result<Value> {
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let cursor = if has_more {
        let row = rows.last().ok_or(QueryError::Database)?;
        Some(encode_query_cursor(
            row["created_at"].as_str().ok_or(QueryError::Database)?,
            row[id].as_str().ok_or(QueryError::Database)?,
        )?)
    } else {
        None
    };
    Ok(json!({key:rows,"next_cursor":cursor,"has_more":has_more}))
}
impl QueryStore {
    pub async fn list_query_history(
        &self,
        user: &UserContext,
        id: &str,
        filter: QueryHistoryFilter,
    ) -> Result<Value> {
        if user.anonymous() {
            return Err(QueryError::Forbidden);
        }
        let dataset = self.require_dataset_access(user, id, "editor").await?;
        if !(1..=100).contains(&filter.limit)
            || filter.mode.as_ref().is_some_and(|m| m.chars().count() > 32)
            || filter.cursor.as_ref().is_some_and(|c| c.len() > 1024)
        {
            return Err(invalid("invalid query-history bounds"));
        }
        let mode = filter.mode.as_deref().unwrap_or("").trim().to_lowercase();
        if !["", "dense", "bm25", "hybrid", "keyword", "vector"].contains(&mode.as_str()) {
            return Err(invalid("unsupported query-history mode"));
        }
        let mut q = QueryBuilder::<Postgres>::new(
            "SELECT to_jsonb(page) FROM (SELECT q.id,q.dataset_id,q.content,q.source,q.source_app_id,q.created_by_role,q.created_by,q.metadata,q.trace_id,q.query_fingerprint,q.mode,q.top_k,q.hit_count,q.stage_timings,q.created_at FROM dataset_queries q JOIN datasets d ON d.dataset_id=q.dataset_id WHERE q.dataset_id=",
        );
        q.push_bind(id.to_owned())
            .push(" AND d.tenant_id=")
            .push_bind(dataset.tenant_id)
            .push(" AND d.is_deleted=FALSE");
        if let Some(zero) = filter.zero_results {
            q.push(if zero {
                " AND q.hit_count=0"
            } else {
                " AND q.hit_count>0"
            });
        }
        if !mode.is_empty() {
            q.push(" AND q.mode=").push_bind(mode);
        }
        if let Some(cursor) = filter.cursor.as_deref().filter(|s| !s.is_empty()) {
            let (time, row) = decode_query_cursor(cursor)?;
            q.push(" AND(q.created_at,q.id)<(")
                .push_bind(time)
                .push(",")
                .push_bind(row)
                .push(")");
        }
        q.push(" ORDER BY q.created_at DESC,q.id DESC LIMIT ")
            .push_bind((filter.limit + 1) as i64)
            .push(") page");
        let rows: Vec<Value> = q.build_query_scalar().fetch_all(&self.pool).await?;
        page(rows, filter.limit, "id", "queries")
    }
    pub async fn upsert_query_feedback(
        &self,
        user: &UserContext,
        id: &str,
        payload: FeedbackUpsert,
    ) -> std::result::Result<Value, ObservationError> {
        payload.validate()?;
        if user.anonymous() {
            return Err(QueryError::Forbidden.into());
        }
        let dataset = self.require_dataset_access(user, id, "viewer").await?;
        let trace = payload.trace_id.to_string();
        let observed:Option<Option<String>>=sqlx::query_scalar("SELECT q.query_fingerprint FROM dataset_queries q JOIN datasets d ON d.dataset_id=q.dataset_id WHERE q.dataset_id=$1 AND d.tenant_id=$2 AND d.is_deleted=FALSE AND q.trace_id=$3::uuid").bind(id).bind(&dataset.tenant_id).bind(&trace).fetch_optional(&self.pool).await?;
        if observed
            .flatten()
            .is_some_and(|f| !f.is_empty() && f != payload.query_fingerprint)
        {
            return Err(ObservationError::Conflict);
        }
        let target = if payload.target_type == "retrieval_hit" {
            payload.segment_id.as_deref().unwrap_or("")
        } else {
            &trace
        };
        let comment = payload
            .comment
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let mut row:Value=sqlx::query_scalar("WITH saved AS(INSERT INTO knowledge.dataset_query_feedback(tenant_id,dataset_id,trace_id,query_fingerprint,target_type,target_id,rating,reason_code,comment,created_by) VALUES($1,$2,$3::uuid,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT(tenant_id,dataset_id,trace_id,target_type,target_id,created_by) DO UPDATE SET query_fingerprint=EXCLUDED.query_fingerprint,rating=EXCLUDED.rating,reason_code=EXCLUDED.reason_code,comment=EXCLUDED.comment,updated_at=NOW() RETURNING *) SELECT to_jsonb(saved) FROM saved").bind(&dataset.tenant_id).bind(id).bind(&trace).bind(&payload.query_fingerprint).bind(&payload.target_type).bind(target).bind(&payload.rating).bind(&payload.reason_code).bind(comment).bind(&user.user_id).fetch_one(&self.pool).await?;
        row["query_content"] = Value::Null;
        Ok(row)
    }
    pub async fn list_query_feedback(
        &self,
        user: &UserContext,
        id: &str,
        filter: FeedbackFilter,
    ) -> Result<Value> {
        if user.anonymous() {
            return Err(QueryError::Forbidden);
        }
        let dataset = self.require_dataset_access(user, id, "editor").await?;
        if !(1..=100).contains(&filter.limit)
            || filter.cursor.as_ref().is_some_and(|c| c.len() > 1024)
            || filter
                .reason_code
                .as_ref()
                .is_some_and(|s| s.chars().count() > 32)
            || filter
                .rating
                .as_deref()
                .is_some_and(|v| !["positive", "negative"].contains(&v))
            || filter
                .target_type
                .as_deref()
                .is_some_and(|v| !["retrieval_hit", "qa_answer"].contains(&v))
        {
            return Err(invalid("invalid feedback filter"));
        }
        let mut q = QueryBuilder::<Postgres>::new(
            "SELECT to_jsonb(page) FROM(SELECT f.*,q.content AS query_content FROM knowledge.dataset_query_feedback f JOIN datasets d ON d.dataset_id=f.dataset_id LEFT JOIN dataset_queries q ON q.dataset_id=f.dataset_id AND q.trace_id=f.trace_id WHERE d.is_deleted=FALSE AND f.tenant_id=",
        );
        q.push_bind(dataset.tenant_id.clone())
            .push(" AND d.tenant_id=")
            .push_bind(dataset.tenant_id)
            .push(" AND f.dataset_id=")
            .push_bind(id.to_owned());
        for (column, value) in [
            ("rating", filter.rating),
            ("reason_code", filter.reason_code),
            ("target_type", filter.target_type),
        ] {
            if let Some(v) = value.filter(|v| !v.is_empty()) {
                q.push(format!(" AND f.{column}=")).push_bind(v);
            }
        }
        if let Some(trace) = filter.trace_id {
            q.push(" AND f.trace_id=")
                .push_bind(trace.to_string())
                .push("::uuid");
        }
        if let Some(cursor) = filter.cursor.as_deref().filter(|s| !s.is_empty()) {
            let (time, row) = decode_query_cursor(cursor)?;
            q.push(" AND(f.created_at,f.feedback_id)<(")
                .push_bind(time)
                .push(",")
                .push_bind(row)
                .push("::uuid)");
        }
        q.push(" ORDER BY f.created_at DESC,f.feedback_id DESC LIMIT ")
            .push_bind((filter.limit + 1) as i64)
            .push(") page");
        let rows: Vec<Value> = q.build_query_scalar().fetch_all(&self.pool).await?;
        page(rows, filter.limit, "feedback_id", "feedback")
    }
}
