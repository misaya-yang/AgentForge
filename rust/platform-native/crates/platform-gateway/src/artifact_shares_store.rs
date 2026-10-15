//! Canonical artifact share SQL. Every awaited statement is independently
//! supervised; this deliberately does not turn source's reads+insert into one
//! transaction. Public inserts omit audience/source_scope and use schema defaults.
use crate::{
    artifact_shares_api::{ArtifactSharesError, ArtifactSharesFuture, ArtifactSharesIdentity},
    postgres_operation::{PgOperationError, PgOperationStorage},
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
#[derive(Clone)]
pub struct NewArtifactShare {
    pub share_id: String,
    pub share_code: String,
    pub title: String,
    pub payload: Value,
    pub answer_keys: Value,
    pub tenant_id: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_attempts: Option<i64>,
    pub require_name: bool,
    pub time_limit_minutes: Option<i64>,
    pub audience: String,
    pub source_scope: Option<Value>,
}
pub trait ArtifactSharesDatabase: Send + Sync {
    fn owned_quiz<'a>(
        &'a self,
        user: &'a ArtifactSharesIdentity,
        id: &'a str,
    ) -> ArtifactSharesFuture<'a, Option<Value>>;
    fn questions<'a>(&'a self, id: &'a str) -> ArtifactSharesFuture<'a, Vec<Value>>;
    fn insert<'a>(&'a self, share: NewArtifactShare) -> ArtifactSharesFuture<'a, ()>;
    fn list<'a>(
        &'a self,
        user: &'a ArtifactSharesIdentity,
        id: &'a str,
        limit: i64,
    ) -> ArtifactSharesFuture<'a, Vec<Value>>;
    fn revoke<'a>(
        &'a self,
        user: &'a ArtifactSharesIdentity,
        id: &'a str,
    ) -> ArtifactSharesFuture<'a, bool>;
}
pub const ARTIFACT_SHARE_LIST_SQL: &str = "SELECT id::text AS share_id, share_code, is_active, created_at, expires_at, audience, revoked_at, require_name, (expires_at IS NOT NULL AND expires_at <= NOW()) AS expired FROM assistant.artifact_shares WHERE tenant_id = $1 AND created_by = $2 AND kind = 'quiz' AND payload ->> 'quiz_id' = $3 ORDER BY created_at DESC, id DESC LIMIT $4";
pub const ARTIFACT_SHARE_REVOKE_SQL: &str = "UPDATE assistant.artifact_shares SET is_active = FALSE, revoked_at = NOW() WHERE id = $1::text::uuid AND created_by = $2 AND tenant_id = $3";
pub(crate) fn pg_error(error: PgOperationError) -> ArtifactSharesError {
    match error {
        PgOperationError::Cancelled => ArtifactSharesError::Cancelled,
        _ => ArtifactSharesError::Internal,
    }
}
#[derive(Clone)]
pub struct PgArtifactSharesDatabase {
    pub(crate) operations: PgOperationStorage,
}
impl PgArtifactSharesDatabase {
    pub fn new(
        pool: PgPool,
        independent_control_pool: PgPool,
    ) -> Result<Self, ArtifactSharesError> {
        Ok(Self {
            operations: PgOperationStorage::new(pool, independent_control_pool)
                .map_err(pg_error)?,
        })
    }
    pub async fn drain_cancelled(&self) -> Result<(), ArtifactSharesError> {
        self.operations.drain_cancelled().await.map_err(pg_error)
    }
}
impl ArtifactSharesDatabase for PgArtifactSharesDatabase {
    fn owned_quiz<'a>(
        &'a self,
        user: &'a ArtifactSharesIdentity,
        id: &'a str,
    ) -> ArtifactSharesFuture<'a, Option<Value>> {
        let (tenant, user, id) = (user.tenant_id.clone(), user.user_id.clone(), id.to_owned());
        Box::pin(async move {
            self.operations.acquire().await.map_err(pg_error)?.execute(move|connection|Box::pin(async move{sqlx::query_scalar("SELECT jsonb_build_object('id',id::text,'tenant_id',tenant_id,'title',title,'description',description,'question_count',question_count,'difficulty',difficulty,'dataset_ids',dataset_ids) FROM quizzes WHERE id=$1::text::uuid AND tenant_id=$2 AND created_by=$3").bind(id).bind(tenant).bind(user).fetch_optional(connection).await})).await.map_err(pg_error)
        })
    }
    fn questions<'a>(&'a self, id: &'a str) -> ArtifactSharesFuture<'a, Vec<Value>> {
        let id = id.to_owned();
        Box::pin(async move {
            self.operations.acquire().await.map_err(pg_error)?.execute(move|connection|Box::pin(async move{sqlx::query_scalar("SELECT jsonb_build_object('id',id::text,'question_num',question_num,'question_type',question_type,'question_text',question_text,'options',options::text,'correct_answer',correct_answer::text,'explanation',explanation) FROM quiz_questions WHERE quiz_id=$1::text::uuid ORDER BY question_num").bind(id).fetch_all(connection).await})).await.map_err(pg_error)
        })
    }
    fn insert<'a>(&'a self, s: NewArtifactShare) -> ArtifactSharesFuture<'a, ()> {
        Box::pin(async move {
            if (s.audience != "public" && s.audience != "internal")
                || (s.audience == "internal") != s.source_scope.is_some()
                || s.payload.get("source_scope").is_some()
            {
                return Err(ArtifactSharesError::Internal);
            }
            let max = s
                .max_attempts
                .map(i32::try_from)
                .transpose()
                .map_err(|_| ArtifactSharesError::Internal)?;
            let time = s
                .time_limit_minutes
                .map(i32::try_from)
                .transpose()
                .map_err(|_| ArtifactSharesError::Internal)?;
            self.operations.acquire().await.map_err(pg_error)?.execute(move|connection|Box::pin(async move{
   let sql=if s.audience=="internal" {"INSERT INTO assistant.artifact_shares (id,share_code,kind,title,payload,answer_keys,tenant_id,created_by,is_active,max_attempts,expires_at,require_name,time_limit_minutes,created_at,audience,source_scope) VALUES ($1::text::uuid,$2,'quiz',$3,$4,$5,$6,$7,TRUE,$8,$9,$10,$11,$12,$13,$14::jsonb)"} else {"INSERT INTO assistant.artifact_shares (id,share_code,kind,title,payload,answer_keys,tenant_id,created_by,is_active,max_attempts,expires_at,require_name,time_limit_minutes,created_at) VALUES ($1::text::uuid,$2,'quiz',$3,$4,$5,$6,$7,TRUE,$8,$9,$10,$11,$12)"};
   let query=sqlx::query(sql).bind(s.share_id).bind(s.share_code).bind(s.title).bind(s.payload).bind(s.answer_keys).bind(s.tenant_id).bind(s.created_by).bind(max).bind(s.expires_at).bind(s.require_name).bind(time).bind(s.created_at);
   let query=if s.audience=="internal" {query.bind(s.audience).bind(s.source_scope)}else{query};query.execute(connection).await?;Ok(())
  })).await.map_err(pg_error)
        })
    }
    fn list<'a>(
        &'a self,
        user: &'a ArtifactSharesIdentity,
        id: &'a str,
        limit: i64,
    ) -> ArtifactSharesFuture<'a, Vec<Value>> {
        let (tenant, user, id) = (user.tenant_id.clone(), user.user_id.clone(), id.to_owned());
        Box::pin(async move {
            self.operations.acquire().await.map_err(pg_error)?.execute(move|connection|Box::pin(async move{
  let rows=sqlx::query(ARTIFACT_SHARE_LIST_SQL).bind(tenant).bind(user).bind(id).bind(limit).fetch_all(connection).await?;
  rows.into_iter().map(|r|Ok(json!({"share_id":r.try_get::<String,_>("share_id")?,"share_code":r.try_get::<String,_>("share_code")?,"is_active":r.try_get::<bool,_>("is_active")?,"expired":r.try_get::<bool,_>("expired")?,"created_at":r.try_get::<DateTime<Utc>,_>("created_at")?,"expires_at":r.try_get::<Option<DateTime<Utc>>,_>("expires_at")?,"revoked_at":r.try_get::<Option<DateTime<Utc>>,_>("revoked_at")?,"require_name":r.try_get::<bool,_>("require_name")?,"audience":r.try_get::<String,_>("audience")?}))).collect::<Result<Vec<_>,sqlx::Error>>()
 })).await.map_err(pg_error)
        })
    }
    fn revoke<'a>(
        &'a self,
        user: &'a ArtifactSharesIdentity,
        id: &'a str,
    ) -> ArtifactSharesFuture<'a, bool> {
        let (tenant, user, id) = (user.tenant_id.clone(), user.user_id.clone(), id.to_owned());
        Box::pin(async move {
            if tenant.is_none() {
                return Err(ArtifactSharesError::Internal);
            }
            self.operations
                .acquire()
                .await
                .map_err(pg_error)?
                .execute(move |connection| {
                    Box::pin(async move {
                        Ok(sqlx::query(ARTIFACT_SHARE_REVOKE_SQL)
                            .bind(id)
                            .bind(user)
                            .bind(tenant)
                            .execute(connection)
                            .await?
                            .rows_affected()
                            == 1)
                    })
                })
                .await
                .map_err(pg_error)
        })
    }
}
