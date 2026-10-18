use super::storage::{ObjectInfo, ObjectStorage};
use super::{
    ContentAccess, ContentDatabase, ContentError, ContentIdentity, attachment_content_disposition,
    hex_id, sha256,
};
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

fn raw() -> String {
    "raw".into()
}
fn ai() -> String {
    "ai".into()
}
fn object() -> Value {
    json!({})
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub artifact_id: String,
    pub session_id: String,
    pub tenant_id: String,
    pub user_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub format: String,
    pub title: String,
    pub filename: String,
    pub storage_key: String,
    #[serde(default)]
    pub size_bytes: i64,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default = "ai")]
    pub source: String,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default = "object")]
    pub metadata: Value,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default = "raw")]
    pub variant: String,
    #[serde(default)]
    pub parent_artifact_id: Option<String>,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub owner_scope: Option<String>,
    #[serde(default)]
    pub width: Option<i32>,
    #[serde(default)]
    pub height: Option<i32>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct NewArtifact {
    pub session_id: String,
    pub tenant_id: String,
    pub user_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub format: String,
    pub title: String,
    pub filename: String,
    #[serde(default = "ai")]
    pub source: String,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default = "object")]
    pub metadata: Value,
    #[serde(default = "raw")]
    pub variant: String,
    #[serde(default)]
    pub parent_artifact_id: Option<String>,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub owner_scope: Option<String>,
    #[serde(default)]
    pub width: Option<i32>,
    #[serde(default)]
    pub height: Option<i32>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub artifact_id: Option<String>,
}
pub fn mime_type(format: &str) -> &'static str {
    match format.trim_start_matches('.').to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "doc" => "application/msword",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xls" => "application/vnd.ms-excel",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "md" => "text/markdown",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "html" => "text/html",
        "py" => "text/x-python",
        "js" => "text/javascript",
        "ts" => "text/typescript",
        _ => "application/octet-stream",
    }
}
pub fn storage_key(tenant: &str, session: &str, id: &str, filename: &str) -> String {
    let safe: String = filename
        .chars()
        .filter(|c| c.is_alphanumeric() || "._-".contains(*c))
        .collect();
    format!("artifacts/{tenant}/{session}/{id}_{safe}")
}
pub fn matches_owner(
    a: &Artifact,
    owner: Option<&str>,
    tenant: Option<&str>,
    user: Option<&str>,
) -> bool {
    let Some(owner) = owner else { return true };
    if let Some(actual) = a.owner_scope.as_deref().filter(|s| !s.is_empty()) {
        actual == owner
            && tenant.is_none_or(|t| a.tenant_id == t)
            && user.is_none_or(|u| a.user_id == u)
    } else {
        tenant.is_some_and(|t| !t.is_empty() && a.tenant_id == t)
            && user.is_some_and(|u| !u.is_empty() && a.user_id == u)
    }
}
fn from_row(mut value: Value) -> Result<Artifact, ContentError> {
    if value["metadata"].is_string() {
        value["metadata"] = serde_json::from_str(value["metadata"].as_str().unwrap())?;
    }
    if value["variant"].is_null() {
        value["variant"] = json!("raw");
    }
    Ok(serde_json::from_value(value)?)
}
#[derive(Clone)]
pub struct ArtifactStorage {
    pub database: Option<ContentDatabase>,
    pub backend: Arc<dyn ObjectStorage>,
}
impl ArtifactStorage {
    async fn upload_owned(
        &self,
        key: &str,
        content: Vec<u8>,
        mime: &str,
        metadata: BTreeMap<String, String>,
    ) -> Result<String, ContentError> {
        if ai_platform_gateway_foundation::worker_lease::current_effect().is_none() {
            return self.backend.upload(key, content, mime, metadata).await;
        }
        // LocalStorage hides a spawn_blocking write. Retain the real permit in
        // an owned waiter until it finishes, including when the image job loses
        // its lease and its caller is cancelled midway through an upload.
        let backend = self.backend.clone();
        let key = key.to_owned();
        let mime = mime.to_owned();
        ai_platform_gateway_foundation::worker_lease::finish_effect(async move {
            backend.upload(&key, content, &mime, metadata).await
        })
        .await
        .map_err(|_| ContentError::storage())?
    }
    pub async fn get(&self, id: &str) -> Result<Option<Artifact>, ContentError> {
        let Some(db) = &self.database else {
            return Ok(None);
        };
        let id = id.to_owned();
        let row: Option<Value> = db
            .run(move |c| {
                Box::pin(async move {
                    sqlx::query_scalar(
                        "SELECT to_jsonb(a) FROM assistant.artifacts a WHERE artifact_id=$1",
                    )
                    .bind(id)
                    .fetch_optional(c)
                    .await
                })
            })
            .await?;
        row.map(from_row).transpose()
    }
    pub async fn get_scoped(
        &self,
        id: &str,
        owner: Option<&str>,
        tenant: Option<&str>,
        user: Option<&str>,
    ) -> Result<Option<Artifact>, ContentError> {
        Ok(self
            .get(id)
            .await?
            .filter(|a| matches_owner(a, owner, tenant, user)))
    }
    pub async fn list_session(
        &self,
        session: &str,
        tenant: Option<&str>,
    ) -> Result<Vec<Artifact>, ContentError> {
        let Some(db) = &self.database else {
            return Ok(vec![]);
        };
        let session = session.to_owned();
        let tenant = tenant.filter(|s| !s.is_empty()).map(str::to_owned);
        let rows:Vec<Value>=db.run(move|c|Box::pin(async move{sqlx::query_scalar("SELECT to_jsonb(a) FROM assistant.artifacts a WHERE session_id=$1 AND ($2::text IS NULL OR tenant_id=$2) ORDER BY created_at ASC").bind(session).bind(tenant).fetch_all(c).await})).await?;
        rows.into_iter().map(from_row).collect()
    }
    pub async fn save(&self, a: &Artifact) -> Result<(), ContentError> {
        let Some(db) = &self.database else {
            return Ok(());
        };
        let row = serde_json::to_value(a)?;
        let r = row.clone();
        let result=db.run(move|c|Box::pin(async move{sqlx::query("INSERT INTO assistant.artifacts (artifact_id,session_id,message_id,tenant_id,user_id,type,format,title,filename,storage_key,size_bytes,mime_type,source,metadata,created_at,updated_at,variant,parent_artifact_id,turn_id,owner_scope,width,height,provider,model_id,prompt) SELECT artifact_id,session_id,message_id,tenant_id,user_id,type,format,title,filename,storage_key,size_bytes,mime_type,source,metadata,created_at,updated_at,variant,parent_artifact_id,turn_id,owner_scope,width,height,provider,model_id,prompt FROM jsonb_populate_record(NULL::assistant.artifacts,$1::jsonb) ON CONFLICT(artifact_id) DO UPDATE SET updated_at=EXCLUDED.updated_at").bind(r).execute(c).await.map(|_|())})).await;
        if result.as_ref().err().and_then(|e| e.sqlstate.as_deref()) == Some("42703") {
            let mut row = row;
            for (key, field) in [
                ("__owner_scope", "owner_scope"),
                ("__variant", "variant"),
                ("__parent_artifact_id", "parent_artifact_id"),
                ("__turn_id", "turn_id"),
            ] {
                row["metadata"][key] = row[field].clone();
            }
            db.run(move|c|Box::pin(async move{sqlx::query("INSERT INTO assistant.artifacts (artifact_id,session_id,message_id,tenant_id,user_id,type,format,title,filename,storage_key,size_bytes,mime_type,source,metadata,created_at,updated_at) SELECT artifact_id,session_id,message_id,tenant_id,user_id,type,format,title,filename,storage_key,size_bytes,mime_type,source,metadata,created_at,updated_at FROM jsonb_populate_record(NULL::assistant.artifacts,$1::jsonb) ON CONFLICT(artifact_id) DO UPDATE SET updated_at=EXCLUDED.updated_at").bind(row).execute(c).await.map(|_|())})).await
        } else {
            result
        }
    }
    pub async fn create(&self, n: NewArtifact, content: Vec<u8>) -> Result<Artifact, ContentError> {
        let idempotent = n.artifact_id.is_some();
        let id = n
            .artifact_id
            .clone()
            .unwrap_or_else(|| format!("art_{}", &uuid::Uuid::new_v4().simple().to_string()[..16]));
        if !hex_id(&id, "art_", 16) {
            return Err(ContentError::new(400, "artifact_id is invalid"));
        }
        if idempotent
            && !n
                .metadata
                .get("content_sha256")
                .and_then(Value::as_str)
                .is_some_and(|s| hex_id(s, "", 64))
        {
            return Err(ContentError::new(
                400,
                "idempotent artifact requires content_sha256",
            ));
        }
        let key = storage_key(&n.tenant_id, &n.session_id, &id, &n.filename);
        let now = Utc::now();
        let a = Artifact {
            artifact_id: id,
            session_id: n.session_id,
            tenant_id: n.tenant_id,
            user_id: n.user_id,
            kind: n.kind,
            format: n.format.clone(),
            title: n.title,
            filename: n.filename,
            storage_key: key,
            size_bytes: content.len() as i64,
            mime_type: Some(mime_type(&n.format).into()),
            source: n.source,
            message_id: n.message_id,
            metadata: if n.metadata.is_null() {
                json!({})
            } else {
                n.metadata
            },
            created_at: Some(now),
            updated_at: Some(now),
            variant: n.variant,
            parent_artifact_id: n.parent_artifact_id,
            turn_id: n.turn_id,
            owner_scope: n.owner_scope,
            width: n.width,
            height: n.height,
            provider: n.provider,
            model_id: n.model_id,
            prompt: n.prompt,
        };
        if idempotent {
            if let Some(existing) = self.get(&a.artifact_id).await? {
                let mut old = serde_json::to_value(&existing)?;
                let mut new = serde_json::to_value(&a)?;
                for key in ["created_at", "updated_at", "metadata"] {
                    old.as_object_mut().unwrap().remove(key);
                    new.as_object_mut().unwrap().remove(key);
                }
                let metadata_matches = a
                    .metadata
                    .as_object()
                    .is_some_and(|m| m.iter().all(|(k, v)| existing.metadata.get(k) == Some(v)));
                if old != new || !metadata_matches {
                    return Err(ContentError::new(409, "artifact idempotency conflict"));
                }
                return Ok(existing);
            }
        }
        let metadata = BTreeMap::from([
            ("artifact_id".into(), a.artifact_id.clone()),
            ("session_id".into(), a.session_id.clone()),
            ("tenant_id".into(), a.tenant_id.clone()),
            ("type".into(), a.kind.clone()),
        ]);
        self.upload_owned(&a.storage_key, content, a.mime_type.as_deref().unwrap(), metadata)
            .await?;
        self.save(&a).await?;
        Ok(a)
    }
    pub async fn download(&self, id: &str) -> Result<Option<Vec<u8>>, ContentError> {
        let Some(a) = self.get(id).await? else {
            return Ok(None);
        };
        Ok(Some(self.backend.download(&a.storage_key).await?))
    }
    pub async fn delete(&self, id: &str) -> Result<bool, ContentError> {
        let Some(a) = self.get(id).await? else {
            return Ok(false);
        };
        let _ = self.backend.delete(&a.storage_key).await;
        if let Some(db) = &self.database {
            let id = id.to_owned();
            db.run(move |c| {
                Box::pin(async move {
                    sqlx::query("DELETE FROM assistant.artifacts WHERE artifact_id=$1")
                        .bind(id)
                        .execute(c)
                        .await
                        .map(|_| ())
                })
            })
            .await?;
        }
        Ok(true)
    }
    pub async fn delete_session(&self, session: &str) -> Result<usize, ContentError> {
        let mut count = 0;
        for a in self.list_session(session, None).await? {
            if self.delete(&a.artifact_id).await? {
                count += 1
            }
        }
        Ok(count)
    }
    pub async fn find_variant(
        &self,
        id: &str,
        variant: &str,
    ) -> Result<Option<Artifact>, ContentError> {
        let attempt=async{let Some(base)=self.get(id).await?else{return Ok(None)};if base.variant==variant{return Ok(Some(base))}let root=if base.variant=="raw"{base.artifact_id}else if let Some(root)=base.parent_artifact_id{root}else{return Ok(None)};if variant=="raw"{return self.get(&root).await}let Some(db)=&self.database else{return Ok(None)};let variant=variant.to_owned();let row:Option<Value>=db.run(move|c|Box::pin(async move{sqlx::query_scalar("SELECT to_jsonb(a) FROM assistant.artifacts a WHERE parent_artifact_id=$1 AND variant=$2 ORDER BY created_at DESC LIMIT 1").bind(root).bind(variant).fetch_optional(c).await})).await?;row.map(from_row).transpose()}.await;
        match attempt {
            Ok(a) => Ok(a),
            Err(_) => Ok(self
                .get(id)
                .await
                .ok()
                .flatten()
                .filter(|a| a.variant == variant)),
        }
    }
    pub async fn variant_url(
        &self,
        id: &str,
        variant: &str,
        expiry: u32,
        owner: Option<&str>,
        tenant: Option<&str>,
        user: Option<&str>,
    ) -> Result<Option<(String, String)>, ContentError> {
        let order: &[&str] = match variant {
            "thumbnail" => &["thumbnail", "display", "raw"],
            "display" => &["display", "raw"],
            _ => &["raw"],
        };
        for v in order {
            if let Some(a) = self.find_variant(id, v).await? {
                if matches_owner(&a, owner, tenant, user) {
                    if let Some(url) = self
                        .backend
                        .download_url(&a.storage_key, expiry, Some(&a.filename))
                        .await?
                    {
                        return Ok(Some((url, (*v).into())));
                    }
                }
            }
        }
        Ok(None)
    }
    pub async fn read_scoped(
        &self,
        id: &str,
        tenant: &str,
        session: &str,
        user: &str,
        max: usize,
        source: &str,
    ) -> Result<Option<(Artifact, Vec<u8>)>, ContentError> {
        if [id, tenant, session, user].iter().any(|s| s.is_empty())
            || max == 0
            || !matches!(source, "tool_output_spill" | "sse_event_spill")
        {
            return Ok(None);
        }
        let Some(a) = self.get(id).await? else {
            return Ok(None);
        };
        if a.tenant_id != tenant
            || a.session_id != session
            || a.user_id != user
            || a.source != source
            || a.turn_id.is_none()
            || a.size_bytes < 0
            || a.size_bytes as u64 > max as u64
        {
            return Ok(None);
        }
        let content = self.backend.download_bounded(&a.storage_key, max).await?;
        if content.len() > max {
            return Ok(None);
        }
        Ok(Some((a, content)))
    }
    pub fn image_blob_key(owner: &str, id: &str, filename: &str) -> Result<String, ContentError> {
        if !hex_id(id, "iblob_", 24) || owner.is_empty() || owner.chars().any(|c| (c as u32) < 32) {
            return Err(ContentError::new(400, "image blob scope invalid"));
        }
        let safe: String = filename
            .chars()
            .filter(|c| *c != '\0')
            .map(|c| {
                if c.is_alphanumeric() || "._-".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let safe: String = safe.trim_matches(['.', '_']).chars().take(180).collect();
        Ok(format!(
            "image-blobs/{}/{id}/{}",
            &sha256(owner.as_bytes())[..24],
            if safe.is_empty() {
                "reference.png"
            } else {
                &safe
            }
        ))
    }
    fn validate_blob_key(key: &str, max: usize) -> Result<(), ContentError> {
        if !key.starts_with("image-blobs/")
            || key.contains("..")
            || key.contains('\0')
            || !(1..=32 * 1024 * 1024).contains(&max)
        {
            Err(ContentError::new(400, "image blob storage key is invalid"))
        } else {
            Ok(())
        }
    }
    pub async fn inspect_blob(
        &self,
        key: &str,
        max: usize,
    ) -> Result<Option<ObjectInfo>, ContentError> {
        Self::validate_blob_key(key, max)?;
        let info = self.backend.head(key).await?;
        if info
            .as_ref()
            .is_some_and(|i| i.size_bytes == 0 || i.size_bytes > max as u64)
        {
            return Err(ContentError::new(400, "image blob exceeds size limit"));
        }
        Ok(info)
    }
    pub async fn read_blob(&self, key: &str, max: usize) -> Result<Vec<u8>, ContentError> {
        let info = self
            .inspect_blob(key, max)
            .await?
            .ok_or_else(|| ContentError::new(404, "image blob object not found"))?;
        let content = self.backend.download_bounded(key, max).await.map_err(|e| {
            if e.status == 413 {
                ContentError::new(400, "image blob size changed during verification")
            } else {
                e
            }
        })?;
        if content.len() as u64 != info.size_bytes || content.len() > max {
            return Err(ContentError::new(
                400,
                "image blob size changed during verification",
            ));
        }
        Ok(content)
    }
    pub async fn store_blob(
        &self,
        key: &str,
        content: Vec<u8>,
        mime: &str,
        metadata: BTreeMap<String, String>,
        max: usize,
    ) -> Result<(), ContentError> {
        Self::validate_blob_key(key, max)?;
        if content.is_empty() || content.len() > max || !mime.starts_with("image/") {
            return Err(ContentError::new(400, "image blob content is invalid"));
        }
        self.upload_owned(key, content, mime, metadata).await?;
        Ok(())
    }
}
pub fn public_artifact(a: &Artifact, variants: bool) -> Value {
    json!({"artifact_id":a.artifact_id,"session_id":a.session_id,"type":a.kind,"format":a.format,"title":a.title,"filename":a.filename,"size_bytes":a.size_bytes,"ready":a.size_bytes>0,"variant":if variants{a.variant.as_str()}else{"raw"},"parent_artifact_id":if variants{a.parent_artifact_id.clone()}else{None},"mime_type":a.mime_type,"source":a.source,"message_id":a.message_id,"download_url":format!("/api/v1/assistant/artifacts/{}/download",a.artifact_id),"metadata":a.metadata,"created_at":a.created_at})
}
#[derive(Clone)]
pub struct ArtifactApi {
    pub storage: Arc<ArtifactStorage>,
    pub access: Arc<dyn ContentAccess>,
}
impl ArtifactApi {
    pub async fn owned(
        &self,
        user: &ContentIdentity,
        id: &str,
        source_check: bool,
    ) -> Result<Artifact, ContentError> {
        let a = self
            .storage
            .get(id)
            .await
            .map_err(|e| {
                if e.missing_schema() {
                    ContentError::new(404, "Artifact not found")
                } else {
                    e
                }
            })?
            .filter(|a| a.tenant_id == user.tenant_id && a.user_id == user.user_id)
            .ok_or_else(|| ContentError::new(404, "Artifact not found"))?;
        if source_check {
            self.access.authorize_artifact_source(user, &a).await?
        }
        Ok(a)
    }
    pub async fn list(&self, user: &ContentIdentity, session: &str) -> Result<Value, ContentError> {
        self.access.authorize_session(user, session).await?;
        let rows = match self
            .storage
            .list_session(session, Some(&user.tenant_id))
            .await
        {
            Ok(rows) => rows,
            Err(e) if e.missing_schema() => vec![],
            Err(e) => return Err(e),
        };
        let mut output = vec![];
        for a in rows {
            if a.source == "user" || a.variant != "raw" {
                continue;
            }
            match self.access.authorize_artifact_source(user, &a).await {
                Ok(()) => (),
                Err(e) if e.status == 403 => continue,
                Err(e) => return Err(e),
            }
            output.push(public_artifact(&a, true));
        }
        Ok(json!({"total":output.len(),"artifacts":output}))
    }
    pub async fn create(
        &self,
        user: &ContentIdentity,
        mut body: Value,
    ) -> Result<Value, ContentError> {
        let session = super::required(&body, "session_id")?;
        self.access.authorize_session(user, &session).await?;
        let encoded = super::required(&body, "content_base64")?;
        let content = STANDARD
            .decode(
                encoded
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>(),
            )
            .map_err(|_| ContentError::new(400, "Invalid base64 content"))?;
        body["tenant_id"] = json!(user.tenant_id);
        body["user_id"] = json!(user.user_id);
        if let Some(fields) = body.as_object_mut() {
            for key in [
                "artifact_id",
                "variant",
                "parent_artifact_id",
                "turn_id",
                "owner_scope",
                "width",
                "height",
                "provider",
                "model_id",
                "prompt",
            ] {
                fields.remove(key);
            }
        }
        let n: NewArtifact = serde_json::from_value(body)?;
        Ok(public_artifact(
            &self.storage.create(n, content).await?,
            false,
        ))
    }
}
pub fn router(api: Arc<ArtifactApi>) -> Router {
    Router::new()
        .route("/assistant/sessions/{session_id}/artifacts", get(list))
        .route("/assistant/artifacts", post(create))
        .route(
            "/assistant/artifacts/{artifact_id}",
            get(read).delete(delete),
        )
        .route("/assistant/artifacts/{artifact_id}/download", get(download))
        .with_state(api)
}
async fn list(
    State(api): State<Arc<ArtifactApi>>,
    Path(session): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = api.access.resolve(&headers).await?;
    Ok(Json(api.list(&u, &session).await?))
}
async fn read(
    State(api): State<Arc<ArtifactApi>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = api.access.resolve(&headers).await?;
    Ok(Json(public_artifact(
        &api.owned(&u, &id, true).await?,
        false,
    )))
}
async fn create(
    State(api): State<Arc<ArtifactApi>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ContentError> {
    let u = api.access.resolve(&headers).await?;
    Ok(Json(api.create(&u, body).await?))
}
async fn delete(
    State(api): State<Arc<ArtifactApi>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = api.access.resolve(&headers).await?;
    api.owned(&u, &id, false).await?;
    api.storage.delete(&id).await?;
    Ok(Json(json!({"artifact_id":id,"status":"deleted"})))
}
async fn download(
    State(api): State<Arc<ArtifactApi>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ContentError> {
    let u = api.access.resolve(&headers).await?;
    let a = api.owned(&u, &id, true).await?;
    let bytes = api
        .storage
        .download(&id)
        .await?
        .ok_or_else(|| ContentError::new(404, "Artifact content not found"))?;
    if bytes.is_empty() {
        return Err(ContentError::new(409, "Artifact is empty or unfinished"));
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(
            "Content-Type",
            a.mime_type.as_deref().unwrap_or("application/octet-stream"),
        )
        .header("Content-Length", bytes.len())
        .header(
            "Content-Disposition",
            attachment_content_disposition(&a.filename),
        )
        .body(Body::from(bytes))
        .map_err(|_| ContentError::new(500, "Failed to download artifact"))
}
