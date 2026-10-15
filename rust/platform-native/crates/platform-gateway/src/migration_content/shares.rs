//! Frozen conversation sharing, reusing accepted quiz grading and source policy.
use super::artifacts::ArtifactStorage;
use super::{
    ContentAccess, ContentDatabase, ContentError, ContentFuture, ContentIdentity,
    attachment_content_disposition, sha256, string,
};
use crate::{
    artifact_share_scope::{ArtifactShareScope, freeze_source_scope},
    artifact_shares_api::{
        ArtifactSharesError, ArtifactSharesIdentity, ArtifactSharesRandom, safe_options,
    },
    session_history::{
        SharingSourceContext, SharingSourceSnapshot, SourceReferences, sharing_source_map,
    },
};
use axum::{
    Extension, Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sqlx::Row;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};
fn share_error(e: ArtifactSharesError) -> ContentError {
    match e {
        ArtifactSharesError::Http(status, value) => {
            ContentError::new(status, value.get("detail").cloned().unwrap_or(value))
        }
        _ => ContentError::new(503, "Share source verification unavailable"),
    }
}
fn identity(u: &ContentIdentity) -> ArtifactSharesIdentity {
    ArtifactSharesIdentity {
        user_id: u.user_id.clone(),
        tenant_id: Some(u.tenant_id.clone()),
        is_authenticated: u.is_authenticated,
        roles: u.roles.clone(),
        permissions: u.permissions.clone(),
        tier: u.tier.clone(),
    }
}
fn decoded(v: Value) -> Result<Value, ContentError> {
    if let Value::String(s) = v {
        Ok(serde_json::from_str(&s)?)
    } else {
        Ok(v)
    }
}
fn extend(a: &mut SourceReferences, b: &SourceReferences) {
    a.datasets.extend(b.datasets.iter().cloned());
    a.documents.extend(b.documents.iter().cloned());
    a.versions.extend(b.versions.iter().cloned());
}
fn private(s: &SourceReferences) -> bool {
    !s.datasets.is_empty() || !s.documents.is_empty() || !s.versions.is_empty()
}
pub fn strip_snapshot(snapshot: &Value) -> Value {
    let mut v = snapshot.clone();
    if let Some(m) = v.as_object_mut() {
        m.remove("quiz_answer_keys");
    }
    v
}
pub fn require_safe_snapshot(snapshot: &Value, audience: &str) -> Result<(), ContentError> {
    let (version, policy) = if audience == "internal" {
        (3, "verified_internal_source_scope")
    } else {
        (2, "verified_no_private_knowledge")
    };
    if snapshot["share_snapshot_version"] != version || snapshot["source_policy"] != policy {
        Err(ContentError::new(
            410,
            "This older share needs a new source-rights review",
        ))
    } else {
        Ok(())
    }
}
/// Python json.dumps(sort_keys=True, ensure_ascii=False), with its spaced separators.
pub fn snapshot_hash(
    snapshot: &Value,
    include: bool,
    expires: Option<i64>,
    audience: &str,
    scope: &Value,
) -> Result<String, ContentError> {
    Ok(sha256(super::python_json(&json!({"snapshot":snapshot,"include_artifacts":include,"expires_days":expires,"audience":audience,"source_scope":scope}),false,true).as_bytes()))
}
pub trait ShareRuntimeHistory: Send + Sync {
    /// None means no runtime thread. The count must include the entire history.
    fn history<'a>(
        &'a self,
        user: &'a ContentIdentity,
        session: &'a str,
        limit: usize,
    ) -> ContentFuture<'a, Option<(Vec<Value>, usize)>>;
    fn public_submit_rate_limit<'a>(&'a self, headers: &'a HeaderMap) -> ContentFuture<'a, ()>;
}
/// Set only by trusted anonymous identity / client-IP middleware.
#[derive(Clone, Default)]
pub struct ShareViewer {
    pub anonymous_id: Option<String>,
    pub client_ip: String,
}
impl ShareViewer {
    fn anonymous(&self) -> String {
        let value = self
            .anonymous_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.client_ip);
        let value = if value.is_empty() { "anonymous" } else { value };
        value.chars().take(128).collect()
    }
}
#[derive(Clone)]
pub struct ConversationShares {
    pub database: ContentDatabase,
    pub artifacts: Arc<ArtifactStorage>,
    pub access: Arc<dyn ContentAccess>,
    pub sources: Arc<dyn ArtifactShareScope>,
    pub history: Arc<dyn ShareRuntimeHistory>,
    pub random: Arc<dyn ArtifactSharesRandom>,
}
impl ConversationShares {
    async fn source_history(
        &self,
        user: &ContentIdentity,
        session: &str,
        history: &Value,
    ) -> Result<(SourceReferences, Vec<Value>), ContentError> {
        let (t, u, s) = (
            user.tenant_id.clone(),
            user.user_id.clone(),
            session.to_owned(),
        );
        let snapshots:Vec<SharingSourceSnapshot>=self.database.run(move|c|Box::pin(async move{let rows=sqlx::query("SELECT run_id::text AS run_id,snapshot,created_at FROM assistant_runtime_snapshots WHERE session_id=$1 AND tenant_id=$2 AND user_id=$3 ORDER BY created_at,snapshot_id").bind(s).bind(t).bind(u).fetch_all(c).await?;rows.into_iter().map(|r|Ok((r.try_get::<String,_>("run_id")?,r.try_get("snapshot")?,r.try_get("created_at")?))).collect()})).await?;
        let snapshots = snapshots
            .into_iter()
            .map(|(r, v, d)| Ok((r, decoded(v)?, d)))
            .collect::<Result<Vec<_>, ContentError>>()?;
        let (t, u, s) = (
            user.tenant_id.clone(),
            user.user_id.clone(),
            session.to_owned(),
        );
        let contexts:Vec<SharingSourceContext>=self.database.run(move|c|Box::pin(async move{let rows=sqlx::query("SELECT turn_id::text AS run_id,payload #> '{data,chunks}' AS chunks,created_at FROM assistant_runtime_items WHERE session_id=$1 AND tenant_id=$2 AND user_id=$3 AND event_type='compat/v1/context_retrieved' ORDER BY sequence").bind(s).bind(t).bind(u).fetch_all(c).await?;rows.into_iter().map(|r|Ok((r.try_get("run_id")?,r.try_get("chunks")?,r.try_get("created_at")?))).collect()})).await?;
        let contexts = contexts
            .into_iter()
            .map(|(r, v, d)| Ok((r, v.map(decoded).transpose()?, d)))
            .collect::<Result<Vec<_>, ContentError>>()?;
        let (references, _) =
            sharing_source_map(history, snapshots.clone(), contexts).map_err(|_| {
                ContentError::new(503, json!({"code":"ASSISTANT_SOURCE_CHECK_UNAVAILABLE"}))
            })?;
        Ok((
            references,
            snapshots
                .into_iter()
                .map(|(run, snapshot, _)| json!({"run_id":run,"snapshot":snapshot}))
                .collect(),
        ))
    }
    async fn quizzes(
        &self,
        user: &ContentIdentity,
        messages: &[Value],
        audience: &str,
    ) -> Result<(Value, Value, SourceReferences), ContentError> {
        let mut public = json!({});
        let mut keys = json!({});
        let mut refs = SourceReferences::default();
        let mut seen = HashSet::new();
        for message in messages {
            let Some(id) = message["metadata"]["quiz_id"]
                .as_str()
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            if !seen.insert(id.to_owned()) {
                continue;
            }
            let id = uuid::Uuid::parse_str(id)
                .map_err(|_| {
                    ContentError::new(409, "A referenced quiz cannot be verified for sharing")
                })?
                .to_string();
            let (t, u, q) = (user.tenant_id.clone(), user.user_id.clone(), id.clone());
            let quiz:Option<Value>=self.database.run(move|c|Box::pin(async move{sqlx::query_scalar("SELECT to_jsonb(q) FROM quizzes q WHERE id=$1::text::uuid AND tenant_id=$2 AND created_by=$3").bind(q).bind(t).bind(u).fetch_optional(c).await})).await?;
            let quiz = quiz.ok_or_else(|| {
                ContentError::new(409, "A referenced quiz cannot be verified for sharing")
            })?;
            let datasets = decoded(
                quiz.get("dataset_ids")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or(json!([])),
            )?;
            let ids = datasets.as_array().ok_or_else(|| {
                ContentError::new(409, "Quiz source rights cannot be verified for sharing")
            })?;
            if ids.iter().any(|v| v.as_str().is_none_or(str::is_empty)) {
                return Err(ContentError::new(
                    409,
                    "Quiz source rights cannot be verified for sharing",
                ));
            }
            if audience == "public" && !ids.is_empty() {
                return Err(ContentError::new(
                    409,
                    "Quiz content derived from private knowledge cannot be shared anonymously",
                ));
            }
            let inherited = self
                .sources
                .quiz_sources(&identity(user), &id)
                .await
                .map_err(|_| {
                    ContentError::new(409, "Quiz source rights cannot be verified for sharing")
                })?;
            if audience == "public" && private(&inherited) {
                return Err(ContentError::new(
                    409,
                    "Quiz content derived from knowledge cannot be shared anonymously",
                ));
            }
            refs.datasets
                .extend(ids.iter().filter_map(Value::as_str).map(str::to_owned));
            extend(&mut refs, &inherited);
            let q = id.clone();
            let questions:Vec<Value>=self.database.run(move|c|Box::pin(async move{sqlx::query_scalar("SELECT to_jsonb(q) FROM quiz_questions q WHERE quiz_id=$1::text::uuid ORDER BY question_num").bind(q).fetch_all(c).await})).await?;
            if questions.is_empty() {
                return Err(ContentError::new(
                    409,
                    "A referenced quiz has no verifiable questions",
                ));
            }
            let (mut public_questions, mut grading) = (vec![], vec![]);
            for q in questions {
                let options = decoded(q["options"].clone()).unwrap_or(json!([]));
                let correct = decoded(q["correct_answer"].clone()).unwrap_or(json!([]));
                public_questions.push(json!({"id":q["id"],"question_num":q["question_num"],"question_type":q["question_type"],"question_text":q["question_text"],"options":safe_options(&options)}));
                grading.push(json!({"id":q["id"],"question_num":q["question_num"],"question_type":q["question_type"],"correct_answer":correct,"explanation":q.get("explanation").filter(|v|!v.is_null()).cloned().unwrap_or(json!(""))}));
            }
            public[&id] = json!({"quiz_id":id,"title":quiz["title"],"description":quiz["description"],"topic":quiz["topic"],"difficulty":quiz["difficulty"],"question_count":quiz["question_count"],"questions":public_questions});
            keys[&id] = json!({"questions":grading});
        }
        Ok((public, keys, refs))
    }
    pub async fn build_snapshot(
        &self,
        user: &ContentIdentity,
        session: &str,
        include: bool,
        audience: &str,
    ) -> Result<(Value, String, Value), ContentError> {
        if !matches!(audience, "public" | "internal") {
            return Err(ContentError::new(422, "Invalid share audience"));
        }
        let (t, u, s) = (
            user.tenant_id.clone(),
            user.user_id.clone(),
            session.to_owned(),
        );
        let row:Option<Value>=self.database.run(move|c|Box::pin(async move{sqlx::query_scalar("SELECT to_jsonb(s) FROM assistant.sessions s WHERE session_id=$1 AND user_id=$2 AND tenant_id=$3").bind(s).bind(u).bind(t).fetch_optional(c).await})).await?;
        let row = row.ok_or_else(|| ContentError::new(404, "Session not found"))?;
        let legacy = decoded(
            row.get("history")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or(json!([])),
        )?;
        let mut history = if legacy.is_object() {
            legacy["messages"].as_array()
        } else {
            legacy.as_array()
        }
        .filter(|a| a.len() <= 1000)
        .cloned()
        .ok_or_else(|| {
            ContentError::new(409, "Conversation history cannot be verified for sharing")
        })?;
        for m in &history {
            if !m.is_object() {
                return Err(ContentError::new(
                    409,
                    "Conversation history cannot be verified for sharing",
                ));
            }
            if m["role"] == "assistant" && m["metadata"]["source_kind"] != "image_generation" {
                return Err(ContentError::new(
                    409,
                    "Older assistant messages have unverified source rights",
                ));
            }
        }
        let (mut sources, snapshots) = self.source_history(user, session, &legacy).await?;
        if audience == "public" && private(&sources) {
            return Err(ContentError::new(
                409,
                "Knowledge-derived content cannot be shared anonymously",
            ));
        }
        if let Some((messages, total)) = self.history.history(user, session, 1000).await? {
            if total > messages.len() {
                return Err(ContentError::new(
                    409,
                    "Conversation is too long to preview completely",
                ));
            }
            let mut runs = HashSet::new();
            for snapshot in snapshots {
                let items = snapshot["snapshot"]["readonly_capabilities"]["items"]
                    .as_array()
                    .ok_or_else(|| {
                        ContentError::new(409, "Conversation source rights cannot be verified")
                    })?;
                if audience == "public" && items.iter().any(|v| v["kind"] == "knowledge") {
                    return Err(ContentError::new(
                        409,
                        "Private knowledge content cannot be shared anonymously",
                    ));
                }
                runs.insert(string(&snapshot, "run_id").to_owned());
            }
            for message in &messages {
                if message["metadata"]["runtime_run_id"]
                    .as_str()
                    .is_none_or(|id| !runs.contains(id))
                {
                    return Err(ContentError::new(
                        409,
                        "Conversation source rights cannot be verified",
                    ));
                }
            }
            history.extend(messages);
        }
        if history.is_empty() {
            return Err(ContentError::new(400, "Session has no messages"));
        }
        let artifacts = if include {
            let (t, u, s) = (
                user.tenant_id.clone(),
                user.user_id.clone(),
                session.to_owned(),
            );
            self.database.run(move|c|Box::pin(async move{sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('artifact_id',artifact_id,'type',type,'format',format,'title',title,'filename',filename,'size_bytes',size_bytes,'mime_type',mime_type,'source',source) FROM assistant.artifacts WHERE session_id=$1 AND tenant_id=$2 AND user_id=$3 AND source<>'user' AND size_bytes>0 AND variant='raw' ORDER BY created_at,artifact_id").bind(s).bind(t).bind(u).fetch_all(c).await})).await?
        } else {
            vec![]
        };
        let ids: HashSet<_> = artifacts
            .iter()
            .filter_map(|a| a["artifact_id"].as_str())
            .collect();
        let mut messages = vec![];
        for message in history {
            if !matches!(string(&message, "role"), "user" | "assistant")
                || !message["content"].is_string()
            {
                continue;
            }
            let mut public = json!({"role":message["role"],"content":message["content"]});
            if message["timestamp"].is_string() {
                public["timestamp"] = message["timestamp"].clone()
            }
            if message["role"] == "assistant" && message["metadata"].is_object() {
                let meta = &message["metadata"];
                let mut safe = json!({});
                if meta["quiz_id"].is_string() {
                    safe["quiz_id"] = meta["quiz_id"].clone()
                }
                if let Some(a) = meta["artifact_ids"].as_array() {
                    safe["artifact_ids"] = json!(
                        a.iter()
                            .filter(|v| v.as_str().is_some_and(|id| ids.contains(id)))
                            .cloned()
                            .collect::<Vec<_>>()
                    );
                }
                if !safe.as_object().unwrap().is_empty() {
                    public["metadata"] = safe;
                }
            }
            messages.push(public)
        }
        let (quizzes, keys, quiz_sources) = self.quizzes(user, &messages, audience).await?;
        for message in &mut messages {
            let quiz = message["metadata"]["quiz_id"].as_str().unwrap_or("");
            if quizzes.get(quiz).is_some() {
                message["quiz_data"] = quizzes[quiz].clone();
            }
        }
        let metadata = decoded(row["metadata"].clone())?;
        let title = string(&metadata, "title").to_owned();
        let mut snapshot = json!({"share_snapshot_version":if audience=="internal"{3}else{2},"source_policy":if audience=="internal"{"verified_internal_source_scope"}else{"verified_no_private_knowledge"},"messages":messages,"artifacts":artifacts});
        if keys.as_object().is_some_and(|m| !m.is_empty()) {
            snapshot["quiz_answer_keys"] = keys;
        }
        let scope = if audience == "internal" {
            let identity = identity(user);
            self.sources
                .require_active(&identity)
                .await
                .map_err(share_error)?;
            extend(&mut sources, &quiz_sources);
            let scope = freeze_source_scope(&sources).map_err(share_error)?;
            if !self
                .sources
                .allowed(&identity, &sources)
                .await
                .map_err(share_error)?
            {
                return Err(ContentError::new(
                    403,
                    "Share source access has been revoked",
                ));
            }
            scope
        } else {
            Value::Null
        };
        Ok((snapshot, title, scope))
    }
    pub async fn preview(
        &self,
        user: &ContentIdentity,
        session: &str,
        options: &ShareOptions,
    ) -> Result<Value, ContentError> {
        let (snapshot, title, scope) = self
            .build_snapshot(user, session, options.include_artifacts, &options.audience)
            .await?;
        Ok(
            json!({"title":title,"messages":snapshot["messages"],"artifacts":snapshot["artifacts"],"message_count":snapshot["messages"].as_array().unwrap().len(),"artifact_count":snapshot["artifacts"].as_array().unwrap().len(),"audience":options.audience,"expires_days":options.expires_days,"preview_hash":snapshot_hash(&snapshot,options.include_artifacts,options.expires_days,&options.audience,&scope)?}),
        )
    }
    fn new_code(&self) -> Result<String, ContentError> {
        let alphabet = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let mut code = String::new();
        while code.len() < 8 {
            let mut bytes = [0u8; 16];
            self.random.fill(&mut bytes).map_err(share_error)?;
            for byte in bytes {
                if byte < 252 {
                    code.push(alphabet[(byte % 36) as usize] as char);
                    if code.len() == 8 {
                        break;
                    }
                }
            }
        }
        Ok(code)
    }
    pub async fn create(
        &self,
        user: &ContentIdentity,
        session: &str,
        options: &ShareOptions,
    ) -> Result<Value, ContentError> {
        let (mut snapshot, title, scope) = self
            .build_snapshot(user, session, options.include_artifacts, &options.audience)
            .await?;
        if options.preview_hash.as_deref()
            != Some(&snapshot_hash(
                &snapshot,
                options.include_artifacts,
                options.expires_days,
                &options.audience,
                &scope,
            )?)
        {
            return Err(ContentError::new(
                409,
                "Share preview changed. Review it again before creating a link",
            ));
        }
        let now = Utc::now();
        snapshot["shared_at"] = json!(now.to_rfc3339());
        let mut code = self.new_code()?;
        let mut available = false;
        for _ in 0..5 {
            let q = code.clone();
            let exists: bool = self
                .database
                .run(move |c| {
                    Box::pin(async move {
                        sqlx::query_scalar(
                            "SELECT EXISTS(SELECT 1 FROM conversation_shares WHERE share_code=$1)",
                        )
                        .bind(q)
                        .fetch_one(c)
                        .await
                    })
                })
                .await?;
            if !exists {
                available = true;
                break;
            }
            code = self.new_code()?;
        }
        if !available {
            return Err(ContentError::new(
                409,
                "Could not generate unique share code, try again",
            ));
        }
        let messages = snapshot["messages"].as_array().unwrap().len() as i32;
        let artifacts = snapshot["artifacts"].as_array().unwrap().len() as i32;
        let expires = options.expires_days.map(|d| now + Duration::days(d));
        let (t, u, s, q, a) = (
            user.tenant_id.clone(),
            user.user_id.clone(),
            session.to_owned(),
            code.clone(),
            options.audience.clone(),
        );
        let saved_title = if title.is_empty() {
            format!("Conversation ({messages} messages)")
        } else {
            title.clone()
        };
        self.database.run(move|c|Box::pin(async move{sqlx::query("INSERT INTO conversation_shares(share_code,session_id,user_id,tenant_id,title,snapshot,message_count,artifact_count,expires_at,audience,source_scope) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)").bind(q).bind(s).bind(u).bind(t).bind(saved_title).bind(snapshot).bind(messages).bind(artifacts).bind(expires).bind(a).bind(if scope.is_null(){None}else{Some(scope)}).execute(c).await.map(|_|())})).await?;
        Ok(
            json!({"share_code":code,"share_url":format!("/share/{code}"),"title":title,"message_count":messages,"artifact_count":artifacts,"created_at":now,"expires_at":expires,"audience":options.audience}),
        )
    }
    async fn row(&self, code: &str) -> Result<Value, ContentError> {
        let code = code.to_owned();
        let row: Option<Value> = self
            .database
            .run(move |c| {
                Box::pin(async move {
                    sqlx::query_scalar(
                        "SELECT to_jsonb(s) FROM conversation_shares s WHERE share_code=$1",
                    )
                    .bind(code)
                    .fetch_optional(c)
                    .await
                })
            })
            .await?;
        let row = row
            .filter(|r| r["is_active"] == true)
            .ok_or_else(|| ContentError::new(404, "Share not found"))?;
        if let Some(expiry) = row["expires_at"]
            .as_str()
            .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        {
            if expiry < Utc::now() {
                return Err(ContentError::new(410, "Share has expired"));
            }
        }
        Ok(row)
    }
    async fn audience(
        &self,
        headers: &HeaderMap,
        row: &Value,
    ) -> Result<Option<ContentIdentity>, ContentError> {
        match row
            .get("audience")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("public")
        {
            "public" => Ok(None),
            "internal" => {
                let user = self.access.resolve(headers).await?;
                if !user.is_authenticated || user.user_id.is_empty() || user.tenant_id.is_empty() {
                    return Err(ContentError::new(
                        401,
                        "Sign in to open this internal share",
                    ));
                }
                if row["tenant_id"] != user.tenant_id {
                    return Err(ContentError::new(404, "Share not found"));
                }
                let actor = identity(&user);
                self.sources
                    .require_active(&actor)
                    .await
                    .map_err(share_error)?;
                let scope = crate::quiz_scope::parse_source_scope(&row["source_scope"])
                    .map_err(share_error)?;
                if !self
                    .sources
                    .allowed(&actor, &scope)
                    .await
                    .map_err(share_error)?
                {
                    return Err(ContentError::new(
                        403,
                        "Share source access has been revoked",
                    ));
                }
                Ok(Some(user))
            }
            _ => Err(ContentError::new(410, "Share audience cannot be verified")),
        }
    }
    pub async fn view(
        &self,
        headers: &HeaderMap,
        code: &str,
    ) -> Result<(Value, bool), ContentError> {
        let row = self.row(code).await?;
        let internal = self.audience(headers, &row).await?.is_some();
        let q = code.to_owned();
        let _=self.database.run(move|c|Box::pin(async move{sqlx::query("UPDATE conversation_shares SET view_count=view_count+1 WHERE share_code=$1").bind(q).execute(c).await.map(|_|())})).await;
        let snapshot = decoded(row["snapshot"].clone())?;
        let audience = if internal { "internal" } else { "public" };
        require_safe_snapshot(&snapshot, audience)?;
        Ok((
            json!({"share_code":code,"title":row["title"],"snapshot":strip_snapshot(&snapshot),"message_count":row["message_count"],"artifact_count":row["artifact_count"],"view_count":row["view_count"].as_i64().unwrap_or(0)+1,"created_at":row["created_at"],"expires_at":row["expires_at"],"audience":audience}),
            internal,
        ))
    }
    async fn attempt(
        &self,
        code: &str,
        anon: &str,
        quiz: &str,
    ) -> Result<Option<Value>, ContentError> {
        let (q, a, id) = (code.to_owned(), anon.to_owned(), quiz.to_owned());
        let row:Option<Value>=self.database.run(move|c|Box::pin(async move{sqlx::query_scalar("SELECT result FROM conversation_share_quiz_attempts WHERE share_code=$1 AND anon_id=$2 AND quiz_id=$3::text::uuid").bind(q).bind(a).bind(id).fetch_optional(c).await})).await?;
        row.map(decoded).transpose()
    }
    pub async fn submit(
        &self,
        headers: &HeaderMap,
        viewer: &ShareViewer,
        code: &str,
        quiz: &str,
        body: &Value,
    ) -> Result<(Value, bool), ContentError> {
        self.history.public_submit_rate_limit(headers).await?;
        let row = self.row(code).await?;
        let user = self.audience(headers, &row).await?;
        let snapshot = decoded(row["snapshot"].clone())?;
        require_safe_snapshot(
            &snapshot,
            if user.is_some() { "internal" } else { "public" },
        )?;
        let key = snapshot["quiz_answer_keys"]
            .get(quiz)
            .ok_or_else(|| ContentError::new(404, "Quiz not found in this share"))?;
        let quiz = uuid::Uuid::parse_str(quiz)
            .map_err(|_| ContentError::new(404, "Quiz not found in this share"))?
            .to_string();
        let anon = if let Some(user) = &user {
            format!("user:{}", user.user_id)
        } else {
            viewer.anonymous()
        };
        if let Some(mut cached) = self.attempt(code, &anon, &quiz).await? {
            cached["cached"] = json!(true);
            return Ok((cached, user.is_some()));
        }
        let answers = body["answers"]
            .as_object()
            .filter(|m| m.values().all(Value::is_string))
            .ok_or_else(|| ContentError::new(422, "Invalid quiz answers"))?;
        let mut result = crate::quiz_grading::grade(
            key["questions"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            answers,
        );
        let attempt = uuid::Uuid::new_v4().to_string();
        result["attempt_id"] = json!(attempt);
        let (q, a, id, payload) = (code.to_owned(), anon.clone(), quiz.clone(), result.clone());
        let write=self.database.run(move|c|Box::pin(async move{sqlx::query("INSERT INTO conversation_share_quiz_attempts(id,share_code,anon_id,quiz_id,result) VALUES($1::text::uuid,$2,$3,$4::text::uuid,$5)").bind(attempt).bind(q).bind(a).bind(id).bind(payload).execute(c).await.map(|_|())})).await;
        if write.is_err() {
            if let Some(mut cached) = self.attempt(code, &anon, &quiz).await? {
                cached["cached"] = json!(true);
                return Ok((cached, user.is_some()));
            }
            return Err(ContentError::new(500, "Failed to record attempt"));
        }
        Ok((result, user.is_some()))
    }
    pub async fn download(
        &self,
        headers: &HeaderMap,
        code: &str,
        id: &str,
    ) -> Result<Response, ContentError> {
        let row = self.row(code).await?;
        let internal = self.audience(headers, &row).await?.is_some();
        let snapshot = decoded(row["snapshot"].clone())?;
        require_safe_snapshot(&snapshot, if internal { "internal" } else { "public" })?;
        if !snapshot["artifacts"]
            .as_array()
            .is_some_and(|a| a.iter().any(|a| a["artifact_id"] == id))
        {
            return Err(ContentError::new(404, "Artifact not in this share"));
        }
        let a = self
            .artifacts
            .get(id)
            .await?
            .filter(|a| {
                a.session_id == string(&row, "session_id")
                    && a.tenant_id == string(&row, "tenant_id")
                    && a.user_id == string(&row, "user_id")
                    && a.source != "user"
                    && a.size_bytes > 0
            })
            .ok_or_else(|| ContentError::new(404, "Artifact not found in storage"))?;
        let bytes = self
            .artifacts
            .download(id)
            .await?
            .ok_or_else(|| ContentError::new(404, "Artifact content not found"))?;
        Response::builder()
            .header(
                "Content-Type",
                a.mime_type.unwrap_or("application/octet-stream".into()),
            )
            .header(
                "Content-Disposition",
                attachment_content_disposition(&a.filename),
            )
            .header("Content-Length", bytes.len())
            .header(
                "Cache-Control",
                if internal {
                    "no-store"
                } else {
                    "private, max-age=0"
                },
            )
            .body(Body::from(bytes))
            .map_err(|_| ContentError::new(404, "Artifact not found"))
    }
    pub async fn list(
        &self,
        user: &ContentIdentity,
        limit: i64,
        session: Option<String>,
    ) -> Result<Value, ContentError> {
        let (t, u) = (user.tenant_id.clone(), user.user_id.clone());
        let rows:Vec<Value>=self.database.run(move|c|Box::pin(async move{sqlx::query_scalar("SELECT to_jsonb(s) FROM (SELECT share_code,session_id,title,message_count,artifact_count,view_count,is_active,created_at,expires_at,audience FROM conversation_shares WHERE tenant_id=$1 AND user_id=$2 AND ($3::text IS NULL OR session_id=$3) ORDER BY created_at DESC LIMIT $4) s").bind(t).bind(u).bind(session).bind(limit).fetch_all(c).await})).await?;
        Ok(json!({"shares":rows}))
    }
    pub async fn revoke(&self, user: &ContentIdentity, code: &str) -> Result<Value, ContentError> {
        let (t, u, q) = (
            user.tenant_id.clone(),
            user.user_id.clone(),
            code.to_owned(),
        );
        let n=self.database.run(move|c|Box::pin(async move{sqlx::query("UPDATE conversation_shares SET is_active=FALSE WHERE share_code=$1 AND tenant_id=$2 AND user_id=$3 AND is_active=TRUE").bind(q).bind(t).bind(u).execute(c).await.map(|r|r.rows_affected())})).await?;
        if n == 0 {
            return Err(ContentError::new(404, "Share not found"));
        }
        Ok(json!({"status":"revoked","share_code":code}))
    }
}
#[derive(Clone)]
pub struct ShareOptions {
    pub include_artifacts: bool,
    pub expires_days: Option<i64>,
    pub preview_hash: Option<String>,
    pub audience: String,
}
impl ShareOptions {
    pub fn parse(v: &Value) -> Result<Self, ContentError> {
        let expires = v
            .get("expires_days")
            .filter(|v| !v.is_null())
            .map(crate::rate_config_api::integer)
            .transpose()
            .map_err(|_| ContentError::new(422, "Invalid share expiry"))?;
        if expires.is_some_and(|n| !(1..=365).contains(&n)) {
            return Err(ContentError::new(422, "Invalid share expiry"));
        }
        let audience = v
            .get("audience")
            .and_then(Value::as_str)
            .unwrap_or("public");
        if !matches!(audience, "public" | "internal") {
            return Err(ContentError::new(422, "Invalid share audience"));
        }
        Ok(Self {
            include_artifacts: v
                .get("include_artifacts")
                .map(crate::rate_config_api::boolean)
                .transpose()
                .map_err(|_| ContentError::new(422, "Invalid include_artifacts"))?
                .unwrap_or(true),
            expires_days: expires,
            preview_hash: v
                .get("preview_hash")
                .and_then(Value::as_str)
                .map(str::to_owned),
            audience: audience.into(),
        })
    }
}
pub fn router(service: Arc<ConversationShares>) -> Router {
    Router::new()
        .route(
            "/assistant/sessions/{session_id}/share-preview",
            get(preview),
        )
        .route("/assistant/sessions/{session_id}/share", post(create))
        .route("/assistant/shares", get(list))
        .route("/assistant/shares/{code}", get(view).delete(revoke))
        .route("/assistant/shares/{code}/quiz/{quiz}/submit", post(submit))
        .route("/assistant/shares/{code}/artifact/{id}", get(download))
        .with_state(service)
}
async fn preview(
    State(s): State<Arc<ConversationShares>>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    h: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = s.access.resolve(&h).await?;
    let options = ShareOptions::parse(&serde_json::to_value(q)?)?;
    Ok(Json(s.preview(&u, &id, &options).await?))
}
async fn create(
    State(s): State<Arc<ConversationShares>>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ContentError> {
    let u = s.access.resolve(&h).await?;
    Ok(Json(s.create(&u, &id, &ShareOptions::parse(&body)?).await?))
}
fn response(v: Value, internal: bool) -> Response {
    if internal {
        ([("Cache-Control", "no-store")], Json(v)).into_response()
    } else {
        Json(v).into_response()
    }
}
async fn view(
    State(s): State<Arc<ConversationShares>>,
    Path(code): Path<String>,
    h: HeaderMap,
) -> Result<Response, ContentError> {
    let (v, i) = s.view(&h, &code).await?;
    Ok(response(v, i))
}
async fn submit(
    State(s): State<Arc<ConversationShares>>,
    Path((code, quiz)): Path<(String, String)>,
    h: HeaderMap,
    viewer: Option<Extension<ShareViewer>>,
    Json(body): Json<Value>,
) -> Result<Response, ContentError> {
    let (v, i) = s
        .submit(
            &h,
            &viewer.map(|v| v.0).unwrap_or_default(),
            &code,
            &quiz,
            &body,
        )
        .await?;
    Ok(response(v, i))
}
async fn download(
    State(s): State<Arc<ConversationShares>>,
    Path((code, id)): Path<(String, String)>,
    h: HeaderMap,
) -> Result<Response, ContentError> {
    s.download(&h, &code, &id).await
}
async fn list(
    State(s): State<Arc<ConversationShares>>,
    Query(q): Query<HashMap<String, String>>,
    h: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = s.access.resolve(&h).await?;
    let limit = q
        .get("limit")
        .map(|s| s.parse::<i64>())
        .transpose()
        .map_err(|_| ContentError::new(422, "Invalid limit"))?
        .unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(ContentError::new(422, "Invalid limit"));
    }
    Ok(Json(s.list(&u, limit, q.get("session_id").cloned()).await?))
}
async fn revoke(
    State(s): State<Arc<ConversationShares>>,
    Path(code): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = s.access.resolve(&h).await?;
    Ok(Json(s.revoke(&u, &code).await?))
}

impl ArtifactSharesRandom for crate::quiz_entropy::OsQuizEntropy {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), ArtifactSharesError> {
        crate::quiz_entropy::QuizEntropy::fill_bytes(self, bytes)
            .map_err(|_| ArtifactSharesError::Internal)
    }
}
