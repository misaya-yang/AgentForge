//! PostgreSQL-authoritative image sessions, turns, blobs, tasks and idempotency.
use super::{ContentDatabase, ContentError, ContentIdentity, sha256, string};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
pub fn owner_scope(user: &ContentIdentity) -> Result<String, ContentError> {
    if !user.tenant_id.is_empty() && !user.user_id.is_empty() {
        if user.tenant_id.contains('\u{1f}') || user.user_id.contains('\u{1f}') {
            return Err(ContentError::new(
                422,
                "app_tenant_id / app_user_id must not contain control characters",
            ));
        }
        Ok(format!(
            "{}\u{1f}{}\u{1f}{}",
            user.user_id, user.tenant_id, user.user_id
        ))
    } else {
        Ok(user.user_id.clone())
    }
}
pub fn request_hash(payload: &Value) -> Result<String, ContentError> {
    let mut value = payload.clone();
    if let Some(m) = value.as_object_mut() {
        m.remove("client_request_id");
    }
    Ok(sha256(super::python_json(&value, true, true).as_bytes()))
}
#[derive(Clone)]
pub struct ImageRepository {
    pub database: ContentDatabase,
}
impl ImageRepository {
    pub async fn session(&self, id: &str) -> Result<Option<Value>, ContentError> {
        let id = id.to_owned();
        self.database
            .run(move |c| {
                Box::pin(async move {
                    sqlx::query_scalar(
                        "SELECT to_jsonb(s) FROM assistant.image_sessions s WHERE session_id=$1",
                    )
                    .bind(id)
                    .fetch_optional(c)
                    .await
                })
            })
            .await
    }
    pub async fn upsert_session(
        &self,
        user: &ContentIdentity,
        id: &str,
        owner: &str,
    ) -> Result<(), ContentError> {
        let (t, u, s, o) = (
            user.tenant_id.clone(),
            user.user_id.clone(),
            id.to_owned(),
            owner.to_owned(),
        );
        self.database.run(move|c|Box::pin(async move{sqlx::query("INSERT INTO assistant.image_sessions(session_id,owner_scope,app_user_id,app_tenant_id) VALUES($1,$2,$3,$4) ON CONFLICT(session_id) DO UPDATE SET updated_at=NOW()").bind(s).bind(o).bind(u).bind(t).execute(c).await.map(|_|())})).await
    }
    pub async fn set_locked_style(
        &self,
        id: &str,
        style: Option<String>,
    ) -> Result<(), ContentError> {
        let id = id.to_owned();
        self.database.run(move|c|Box::pin(async move{sqlx::query("UPDATE assistant.image_sessions SET locked_style=$1,updated_at=NOW() WHERE session_id=$2").bind(style).bind(id).execute(c).await.map(|_|())})).await
    }
    pub async fn advance_latest(
        &self,
        session: &str,
        parent: Option<String>,
        artifact: &str,
    ) -> Result<bool, ContentError> {
        let (s, a) = (session.to_owned(), artifact.to_owned());
        self.database.atomic(move|c|Box::pin(async move{let row:Option<Option<String>>=sqlx::query_scalar("SELECT latest_artifact_id FROM assistant.image_sessions WHERE session_id=$1 FOR UPDATE").bind(&s).fetch_optional(&mut *c).await?;if row!=Some(parent){return Ok(false)}sqlx::query("UPDATE assistant.image_sessions SET latest_artifact_id=$1,updated_at=NOW() WHERE session_id=$2").bind(a).bind(s).execute(c).await?;Ok(true)})).await
    }
    pub async fn blob(&self, id: &str, owner: Option<&str>) -> Result<Option<Value>, ContentError> {
        let (i, o) = (id.to_owned(), owner.map(str::to_owned));
        self.database.run(move|c|Box::pin(async move{sqlx::query_scalar("SELECT to_jsonb(b) FROM assistant.image_blobs b WHERE blob_id=$1 AND ($2::text IS NULL OR owner_scope=$2)").bind(i).bind(o).fetch_optional(c).await})).await
    }
    pub async fn create_blob(&self, mut blob: Value) -> Result<bool, ContentError> {
        for key in ["artifact_id", "error", "content_sha256", "byte_size"] {
            if blob.get(key).is_none() {
                blob[key] = Value::Null
            }
        }
        self.database.run(move|c|Box::pin(async move{sqlx::query("INSERT INTO assistant.image_blobs(blob_id,owner_scope,content_sha256,byte_size,mime_type,storage_key,source,status,artifact_id,error) SELECT blob_id,owner_scope,content_sha256,byte_size,mime_type,storage_key,source,status,artifact_id,error FROM jsonb_populate_record(NULL::assistant.image_blobs,$1) ON CONFLICT(blob_id) DO UPDATE SET content_sha256=COALESCE(EXCLUDED.content_sha256,assistant.image_blobs.content_sha256),byte_size=COALESCE(EXCLUDED.byte_size,assistant.image_blobs.byte_size),mime_type=EXCLUDED.mime_type,storage_key=EXCLUDED.storage_key,source=EXCLUDED.source,status=EXCLUDED.status,artifact_id=COALESCE(EXCLUDED.artifact_id,assistant.image_blobs.artifact_id),error=EXCLUDED.error,completed_at=CASE WHEN EXCLUDED.status='ready' THEN NOW() ELSE assistant.image_blobs.completed_at END,updated_at=NOW()").bind(blob).execute(c).await.map(|r|r.rows_affected()==1)})).await
    }
    pub async fn update_blob(
        &self,
        id: &str,
        owner: &str,
        changes: Value,
    ) -> Result<bool, ContentError> {
        let (i, o) = (id.to_owned(), owner.to_owned());
        self.database.run(move|c|Box::pin(async move{sqlx::query("UPDATE assistant.image_blobs SET status=$3->>'status',content_sha256=COALESCE($3->>'content_sha256',content_sha256),byte_size=COALESCE(($3->>'byte_size')::bigint,byte_size),mime_type=COALESCE($3->>'mime_type',mime_type),artifact_id=COALESCE($3->>'artifact_id',artifact_id),error=$3->>'error',completed_at=CASE WHEN $3->>'status'='ready' THEN NOW() ELSE completed_at END,updated_at=NOW() WHERE blob_id=$1 AND owner_scope=$2").bind(i).bind(o).bind(changes).execute(c).await.map(|r|r.rows_affected()==1)})).await
    }
    pub async fn insert_turn(&self, mut turn: Value) -> Result<(), ContentError> {
        if turn["state"].is_null() {
            turn["state"] = turn["status"].clone()
        }
        self.database.run(move|c|Box::pin(async move{sqlx::query("INSERT INTO assistant.image_turns(turn_id,session_id,owner_scope,task_id,prompt,model_id,style,add_watermark,parent_artifact_id,output_artifact_id,status,error,error_code,client_request_id,request_hash,completed_at,thought_signature,provider_text,output_artifact_ids,state) SELECT turn_id,session_id,owner_scope,task_id,prompt,model_id,style,add_watermark,parent_artifact_id,output_artifact_id,status,error,error_code,client_request_id,request_hash,completed_at,thought_signature,provider_text,output_artifact_ids,state FROM jsonb_populate_record(NULL::assistant.image_turns,$1) ON CONFLICT(turn_id) DO UPDATE SET status=EXCLUDED.status,error=EXCLUDED.error,error_code=EXCLUDED.error_code,output_artifact_id=COALESCE(EXCLUDED.output_artifact_id,assistant.image_turns.output_artifact_id),completed_at=COALESCE(EXCLUDED.completed_at,assistant.image_turns.completed_at),thought_signature=COALESCE(EXCLUDED.thought_signature,assistant.image_turns.thought_signature),provider_text=COALESCE(EXCLUDED.provider_text,assistant.image_turns.provider_text),output_artifact_ids=COALESCE(EXCLUDED.output_artifact_ids,assistant.image_turns.output_artifact_ids),state=COALESCE(EXCLUDED.state,EXCLUDED.status)").bind(turn).execute(c).await.map(|_|())})).await
    }
    pub async fn update_turn(
        &self,
        id: &str,
        status: &str,
        output: Option<String>,
        error: Option<String>,
        code: Option<String>,
    ) -> Result<(), ContentError> {
        let (i, s) = (id.to_owned(), status.to_owned());
        self.database.run(move|c|Box::pin(async move{sqlx::query("UPDATE assistant.image_turns SET status=$2,output_artifact_id=COALESCE($3,output_artifact_id),error=$4,error_code=$5,completed_at=CASE WHEN $2 IN ('completed','failed','unknown') THEN NOW() ELSE completed_at END WHERE turn_id=$1").bind(i).bind(s).bind(output).bind(error).bind(code).execute(c).await.map(|_|())})).await
    }
    pub async fn turn_by_task(&self, id: &str) -> Result<Option<Value>, ContentError> {
        let id = id.to_owned();
        self.database
            .run(move |c| {
                Box::pin(async move {
                    sqlx::query_scalar(
                        "SELECT to_jsonb(t) FROM assistant.image_turns t WHERE task_id=$1",
                    )
                    .bind(id)
                    .fetch_optional(c)
                    .await
                })
            })
            .await
    }
    pub async fn turns(
        &self,
        session: &str,
        owner: &str,
        limit: i64,
        cursor: Option<&str>,
    ) -> Result<(Vec<Value>, Option<String>), ContentError> {
        let (s, o) = (session.to_owned(), owner.to_owned());
        let parsed = cursor.and_then(|s| {
            let (ts, id) = s.split_once('|').unwrap_or((s, ""));
            DateTime::parse_from_rfc3339(ts)
                .ok()
                .map(|t| (t.with_timezone(&Utc), id.to_owned()))
        });
        let (ts, id) = parsed
            .map(|(t, id)| (Some(t), id))
            .unwrap_or((None, String::new()));
        let limit = limit.clamp(1, 200);
        let mut rows:Vec<Value>=self.database.run(move|c|Box::pin(async move{sqlx::query_scalar("SELECT to_jsonb(t) FROM assistant.image_turns t WHERE session_id=$1 AND owner_scope=$2 AND ($3::timestamptz IS NULL OR (created_at,turn_id)<($3,$4)) ORDER BY created_at DESC,turn_id DESC LIMIT $5").bind(s).bind(o).bind(ts).bind(id).bind(limit+1).fetch_all(c).await})).await?;
        let next = if rows.len() > limit as usize {
            let last = &rows[limit as usize - 1];
            Some(format!(
                "{}|{}",
                string(last, "created_at"),
                string(last, "turn_id")
            ))
        } else {
            None
        };
        rows.truncate(limit as usize);
        Ok((rows, next))
    }
    pub async fn task(&self, id: &str) -> Result<Option<Value>, ContentError> {
        let id = id.to_owned();
        self.database
            .run(move |c| {
                Box::pin(async move {
                    sqlx::query_scalar(
                        "SELECT to_jsonb(t) FROM assistant.image_tasks t WHERE task_id=$1",
                    )
                    .bind(id)
                    .fetch_optional(c)
                    .await
                })
            })
            .await
    }
    pub async fn reserve(&self, row: Value) -> Result<Value, ContentError> {
        if ["tenant_id", "user_id", "owner_scope", "request_hash"]
            .iter()
            .any(|k| string(&row, k).is_empty())
            || !matches!(string(&row, "status"), "pending" | "running")
        {
            return Err(ContentError::new(422, "image task scope is invalid"));
        }
        self.database.atomic(move|c|Box::pin(async move{let request=string(&row,"client_request_id");if !request.is_empty(){let inserted:Option<Value>=sqlx::query_scalar("INSERT INTO assistant.image_idempotency(owner_scope,client_request_id,request_hash,task_id) VALUES($1,$2,$3,$4) ON CONFLICT(owner_scope,client_request_id) DO NOTHING RETURNING jsonb_build_object('request_hash',request_hash,'task_id',task_id)").bind(string(&row,"owner_scope")).bind(request).bind(string(&row,"request_hash")).bind(string(&row,"task_id")).fetch_optional(&mut *c).await?;if inserted.is_none(){let existing:Value=sqlx::query_scalar("SELECT jsonb_build_object('request_hash',request_hash,'task_id',task_id) FROM assistant.image_idempotency WHERE owner_scope=$1 AND client_request_id=$2").bind(string(&row,"owner_scope")).bind(request).fetch_one(c).await?;return Ok(json!({"state":if existing["request_hash"]==row["request_hash"]{"existing"}else{"conflict"},"task_id":existing["task_id"]}))}}let n=sqlx::query("INSERT INTO assistant.image_tasks(task_id,tenant_id,user_id,runtime_scope_version,owner_scope,status,prompt,model_id,request_payload,progress,turn_id,session_id,parent_artifact_id,client_request_id,request_hash) SELECT task_id,tenant_id,user_id,1,owner_scope,status,prompt,model_id,request_payload,COALESCE(progress,0),turn_id,session_id,parent_artifact_id,client_request_id,request_hash FROM jsonb_populate_record(NULL::assistant.image_tasks,$1) ON CONFLICT(task_id) DO NOTHING").bind(&row).execute(c).await?.rows_affected();if n!=1{return Err(sqlx::Error::Protocol("image task id collision".into()))}Ok(json!({"state":"reserved","task_id":row["task_id"]}))})).await
    }
    pub async fn update_task(&self, id: &str, changes: Value) -> Result<bool, ContentError> {
        let id = id.to_owned();
        self.database.run(move|c|Box::pin(async move{sqlx::query("UPDATE assistant.image_tasks SET status=COALESCE($2->>'status',status),progress=COALESCE(($2->>'progress')::integer,progress),provider=COALESCE($2->>'provider',provider),result=COALESCE(NULLIF($2->'result','null'::jsonb),result),error=$2->>'error',error_code=$2->>'error_code',parent_artifact_id=COALESCE($2->>'parent_artifact_id',parent_artifact_id),output_artifact_id=COALESCE($2->>'output_artifact_id',output_artifact_id),locked_until=CASE WHEN $2->>'locked_seconds' IS NULL THEN locked_until ELSE NOW()+(($2->>'locked_seconds')::int*INTERVAL '1 second') END,attempt_count=attempt_count+CASE WHEN COALESCE(($2->>'increment_attempt')::boolean,FALSE) THEN 1 ELSE 0 END,started_at=CASE WHEN $2->>'status'='running' AND started_at IS NULL THEN NOW() ELSE started_at END,completed_at=CASE WHEN $2->>'status' IN ('completed','failed','dead_letter','unknown') THEN NOW() ELSE completed_at END,updated_at=NOW() WHERE task_id=$1").bind(id).bind(changes).execute(c).await.map(|r|r.rows_affected()==1)})).await
    }
    pub async fn claim_pending(
        &self,
        limit: i64,
        visibility: i32,
    ) -> Result<Vec<Value>, ContentError> {
        self.database.atomic(move|c|Box::pin(async move{sqlx::query("UPDATE assistant.image_tasks SET status='dead_letter',completed_at=NOW(),updated_at=NOW(),error='image task exceeded retry limit',error_code='max_attempts' WHERE runtime_scope_version=1 AND status='claimed' AND locked_until<NOW() AND attempt_count>=max_attempts").execute(&mut *c).await?;sqlx::query("UPDATE assistant.image_tasks SET status='unknown',completed_at=NOW(),updated_at=NOW(),error='image worker lost after provider dispatch',error_code='worker_lost_after_dispatch' WHERE runtime_scope_version=1 AND status='running' AND locked_until IS NOT NULL AND locked_until<NOW()").execute(&mut *c).await?;sqlx::query_scalar("WITH picked AS(SELECT task_id FROM assistant.image_tasks WHERE runtime_scope_version=1 AND (status='pending' OR(status='claimed' AND locked_until<NOW())) AND attempt_count<max_attempts ORDER BY created_at ASC LIMIT $1 FOR UPDATE SKIP LOCKED) UPDATE assistant.image_tasks task SET status='claimed',locked_until=NOW()+($2::int*INTERVAL '1 second'),attempt_count=attempt_count+1,started_at=COALESCE(started_at,NOW()),updated_at=NOW() FROM picked WHERE task.task_id=picked.task_id RETURNING to_jsonb(task)").bind(limit.clamp(1,10)).bind(visibility).fetch_all(c).await})).await
    }
    pub async fn mark_unknown(&self, id: &str, error: &str) -> Result<bool, ContentError> {
        let (i, e) = (id.to_owned(), error.chars().take(1000).collect::<String>());
        self.database.run(move|c|Box::pin(async move{sqlx::query("UPDATE assistant.image_tasks SET status='unknown',completed_at=NOW(),updated_at=NOW(),error=$2,error_code='worker_failed' WHERE task_id=$1 AND status IN ('claimed','running')").bind(i).bind(e).execute(c).await.map(|r|r.rows_affected()==1)})).await
    }
    pub async fn dispatch_claim(&self, row: &Value) -> Result<bool, ContentError> {
        let row = row.clone();
        self.database.run(move|c|Box::pin(async move{sqlx::query("UPDATE assistant.image_tasks SET status='running',locked_until=NOW()+INTERVAL '15 minutes',updated_at=NOW() WHERE task_id=$1 AND tenant_id=$2 AND user_id=$3 AND owner_scope=$4 AND runtime_scope_version=1 AND status='claimed' AND locked_until>NOW()").bind(string(&row,"task_id")).bind(string(&row,"tenant_id")).bind(string(&row,"user_id")).bind(string(&row,"owner_scope")).execute(c).await.map(|r|r.rows_affected()==1)})).await
    }
}

impl ImageRepository {
    pub async fn turn(&self, id: &str) -> Result<Option<Value>, ContentError> {
        let id = id.to_owned();
        self.database
            .run(move |c| {
                Box::pin(async move {
                    sqlx::query_scalar(
                        "SELECT to_jsonb(t) FROM assistant.image_turns t WHERE turn_id=$1",
                    )
                    .bind(id)
                    .fetch_optional(c)
                    .await
                })
            })
            .await
    }
    pub async fn create_task(&self, row: Value) -> Result<bool, ContentError> {
        self.database.run(move|c|Box::pin(async move{sqlx::query("INSERT INTO assistant.image_tasks(task_id,owner_scope,status,prompt,model_id,request_payload,progress,provider,turn_id,session_id,parent_artifact_id,output_artifact_id,client_request_id,request_hash) SELECT task_id,owner_scope,status,prompt,model_id,request_payload,COALESCE(progress,0),provider,turn_id,session_id,parent_artifact_id,output_artifact_id,client_request_id,request_hash FROM jsonb_populate_record(NULL::assistant.image_tasks,$1) ON CONFLICT(task_id) DO NOTHING").bind(row).execute(c).await.map(|r|r.rows_affected()==1)})).await
    }
    pub async fn count_active(&self, owner: Option<&str>) -> Result<i64, ContentError> {
        let owner = owner.map(str::to_owned);
        self.database.run(move|c|Box::pin(async move{sqlx::query_scalar("SELECT COUNT(*) FROM assistant.image_tasks WHERE ($1::text IS NULL OR owner_scope=$1) AND status IN ('pending','running')").bind(owner).fetch_one(c).await})).await
    }
    pub async fn lookup_idempotent(
        &self,
        owner: &str,
        request: &str,
    ) -> Result<Option<Value>, ContentError> {
        let (o, r) = (owner.to_owned(), request.to_owned());
        self.database.run(move|c|Box::pin(async move{sqlx::query_scalar("SELECT jsonb_build_object('request_hash',request_hash,'task_id',task_id,'created_at',created_at) FROM assistant.image_idempotency WHERE owner_scope=$1 AND client_request_id=$2").bind(o).bind(r).fetch_optional(c).await})).await
    }
    pub async fn record_idempotent(
        &self,
        owner: &str,
        request: &str,
        hash: &str,
        task: &str,
    ) -> Result<bool, ContentError> {
        let (o, r, h, t) = (
            owner.to_owned(),
            request.to_owned(),
            hash.to_owned(),
            task.to_owned(),
        );
        self.database.run(move|c|Box::pin(async move{sqlx::query("INSERT INTO assistant.image_idempotency(owner_scope,client_request_id,request_hash,task_id) VALUES($1,$2,$3,$4) ON CONFLICT(owner_scope,client_request_id) DO NOTHING").bind(o).bind(r).bind(h).bind(t).execute(c).await.map(|r|r.rows_affected()==1)})).await
    }
}
