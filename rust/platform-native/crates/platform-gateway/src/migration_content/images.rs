//! Gateway image orchestration; the durable ledger, never an in-process map,
//! owns claiming and idempotency. Pixel transforms and SSRF-pinned fetch are host adapters.
use super::{ContentAccess, ContentError, ContentFuture, ContentIdentity, hex_id, sha256, string};
use super::{
    artifacts::{Artifact, ArtifactStorage, NewArtifact},
    attachments::sniff_image,
    capabilities::{ImageProvider, decode_image},
    images_state::{ImageRepository, owner_scope, request_hash},
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::Utc;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
fn error(status: u16, code: &str) -> ContentError {
    ContentError::new(status, json!({"error_code":code}))
}
pub fn public_image_error(status: &str, code: &str) -> Option<&'static str> {
    if status == "unknown" || code == "outcome_unknown" {
        Some("image generation outcome unknown")
    } else if status == "failed" {
        Some("image generation failed")
    } else {
        None
    }
}
pub fn resolve_style(raw: &str) -> String {
    let s = raw.trim().to_lowercase();
    match s.as_str() {
        "default" | "realistic" | "anime" | "abstract" | "oil_paint" | "watercolor"
        | "3d_render" | "pixel_art" | "sketch" | "comic" => s,
        "photography" | "portrait" | "<photography>" | "<portrait>" => "realistic".into(),
        "3d" | "<3d cartoon>" => "3d_render".into(),
        "oil" | "<oil painting>" => "oil_paint".into(),
        "<anime>" => "anime".into(),
        "<watercolor>" => "watercolor".into(),
        "<sketch>" => "sketch".into(),
        _ => "default".into(),
    }
}
pub fn normalize_request(body: &Value, async_request: bool) -> Result<Value, ContentError> {
    if !body.is_object() {
        return Err(ContentError::new(422, "Invalid image request"));
    }
    let mut v = json!({"prompt":super::required(body,"prompt")?,"model_id":super::required(body,"model_id")?,"style":resolve_style(body.get("style").and_then(Value::as_str).unwrap_or("default")),"size":"1024*1024","n":1,"session_id":null,"reference_artifact_id":null,"reference_blob_id":null,"reference_image":null,"reference_image_url":null,"add_watermark":true,"app_user_id":null,"app_tenant_id":null,"parent_artifact_id":null,"expected_parent_artifact_id":null,"client_request_id":null,"return_variants":null,"allow_branch":false});
    if async_request {
        v["callback_url"] = Value::Null;
    }
    let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
    for k in keys {
        if k == "style" {
            continue;
        }
        if let Some(value) = body.get(&k) {
            v[&k] = value.clone();
        }
    }
    if string(&v, "prompt").is_empty()
        || !v["n"].as_i64().is_some_and(|n| (1..=4).contains(&n))
        || string(&v, "reference_image").chars().count() > 6000000
        || string(&v, "client_request_id").chars().count() > 128
    {
        return Err(ContentError::new(422, "Invalid image request"));
    }
    for k in ["add_watermark", "allow_branch"] {
        v[k] = json!(
            crate::rate_config_api::boolean(&v[k])
                .map_err(|_| ContentError::new(422, "Invalid image request"))?
        );
    }
    Ok(v)
}
pub use ai_platform_gateway_integrations::migration_content::images::{FetchedImage, ImageReferenceFetch};
pub trait ImageTransforms: Send + Sync {
    /// Source watermark.py adaptive 4-corner opacity/ring algorithm. Failure is fatal.
    fn watermark<'a>(&'a self, raw: Vec<u8>) -> ContentFuture<'a, (Vec<u8>, String)>;
    /// Source thumbnail.py: max edge 256, RGB/RGBA, Lanczos, PNG level 6. Failure returns None.
    fn thumbnail<'a>(&'a self, raw: Vec<u8>) -> ContentFuture<'a, Option<Vec<u8>>>;
}
#[derive(Clone)]
pub struct ImageService {
    pub repository: ImageRepository,
    pub artifacts: Arc<ArtifactStorage>,
    pub provider: Arc<dyn ImageProvider>,
    pub provider_model: Option<String>,
    pub supports_references: bool,
    pub fetcher: Arc<dyn ImageReferenceFetch>,
    pub transforms: Arc<dyn ImageTransforms>,
    pub access: Arc<dyn ContentAccess>,
}
impl ImageService {
    fn has_reference(body: &Value, session: &Value) -> bool {
        [
            "reference_artifact_id",
            "reference_blob_id",
            "reference_image",
            "reference_image_url",
            "parent_artifact_id",
        ]
        .iter()
        .any(|k| !string(body, k).is_empty())
            || !string(session, "latest_artifact_id").is_empty()
    }
    fn reference_support(&self, body: &Value, session: &Value) -> Result<(), ContentError> {
        if Self::has_reference(body, session) && !self.supports_references {
            Err(ContentError::new(
                422,
                json!({"error_code":"reference_unsupported","message":"configured image provider does not support reference images"}),
            ))
        } else {
            Ok(())
        }
    }
    async fn reference(
        &self,
        user: &ContentIdentity,
        body: &Value,
        session: &Value,
        owner: &str,
    ) -> Result<Option<(Vec<u8>, String)>, ContentError> {
        let count = [
            "reference_artifact_id",
            "reference_blob_id",
            "reference_image",
            "reference_image_url",
            "parent_artifact_id",
        ]
        .iter()
        .filter(|k| !string(body, k).is_empty())
        .count()
            + usize::from(!string(session, "latest_artifact_id").is_empty());
        if count > 1 {
            return Err(error(422, "invalid_reference"));
        }
        let aid = [
            string(body, "reference_artifact_id"),
            string(body, "parent_artifact_id"),
            string(session, "latest_artifact_id"),
        ]
        .into_iter()
        .find(|s| !s.is_empty());
        let bytes = if let Some(id) = aid {
            let mut a = self
                .artifacts
                .get_scoped(id, Some(owner), Some(&user.tenant_id), Some(&user.user_id))
                .await?
                .ok_or_else(|| error(404, "reference_not_found"))?;
            if a.variant != "raw" {
                a = self
                    .artifacts
                    .find_variant(id, "raw")
                    .await?
                    .filter(|a| {
                        a.tenant_id == user.tenant_id
                            && a.user_id == user.user_id
                            && a.owner_scope
                                .as_deref()
                                .is_none_or(|o| o.is_empty() || o == owner)
                    })
                    .ok_or_else(|| error(404, "reference_not_found"))?;
            }
            self.artifacts
                .backend
                .download_bounded(&a.storage_key, 8 * 1024 * 1024)
                .await
                .map_err(|e| {
                    if e.status == 413 {
                        error(422, "invalid_reference")
                    } else {
                        e
                    }
                })?
        } else if !string(body, "reference_blob_id").is_empty() {
            let blob = self
                .repository
                .blob(string(body, "reference_blob_id"), Some(owner))
                .await?
                .filter(|b| b["status"] == "ready")
                .ok_or_else(|| error(404, "reference_not_found"))?;
            self.artifacts
                .read_blob(string(&blob, "storage_key"), 8 * 1024 * 1024)
                .await?
        } else if !string(body, "reference_image").is_empty() {
            let raw = string(body, "reference_image")
                .split_once(',')
                .map(|(_, s)| s)
                .unwrap_or(string(body, "reference_image"));
            STANDARD
                .decode(raw)
                .map_err(|_| error(422, "invalid_reference"))?
        } else if !string(body, "reference_image_url").is_empty() {
            let fetched = self
                .fetcher
                .fetch(
                    string(body, "reference_image_url"),
                    8 * 1024 * 1024,
                    Duration::from_secs(30),
                )
                .await
                .map_err(|_| error(422, "invalid_reference"))?;
            let declared = fetched
                .content_type
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_lowercase();
            if !declared.is_empty() && sniff_image(&fetched.bytes) != Some(declared.as_str()) {
                return Err(error(422, "invalid_reference"));
            }
            fetched.bytes
        } else {
            return Ok(None);
        };
        let mime = sniff_image(&bytes).ok_or_else(|| error(422, "invalid_reference"))?;
        if bytes.is_empty() || bytes.len() > 8 * 1024 * 1024 {
            return Err(error(422, "invalid_reference"));
        }
        Ok(Some((bytes, mime.into())))
    }
    fn reservation(
        &self,
        user: &ContentIdentity,
        body: &Value,
        owner: &str,
        task: &str,
        turn: &str,
        session: &str,
        status: &str,
        hash: &str,
    ) -> Value {
        json!({"task_id":task,"tenant_id":user.tenant_id,"user_id":user.user_id,"owner_scope":owner,"status":status,"prompt":body["prompt"],"model_id":if status=="running"{self.provider_model.clone().unwrap_or_else(||string(body,"model_id").into())}else{string(body,"model_id").into()},"request_payload":body,"progress":if status=="running"{1}else{0},"turn_id":turn,"session_id":session,"parent_artifact_id":body["parent_artifact_id"],"client_request_id":body["client_request_id"],"request_hash":hash})
    }
    pub async fn submit(&self, user: &ContentIdentity, body: Value) -> Result<Value, ContentError> {
        let owner = owner_scope(user)?;
        let task = format!("imt_{}", &uuid::Uuid::new_v4().simple().to_string()[..20]);
        let turn = format!("itn_{}", &uuid::Uuid::new_v4().simple().to_string()[..16]);
        let session = body
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("img_{}", uuid::Uuid::new_v4().simple()));
        if self
            .repository
            .session(&session)
            .await?
            .is_some_and(|s| s["owner_scope"] != owner)
        {
            return Err(error(404, "not_found"));
        }
        self.repository
            .upsert_session(user, &session, &owner)
            .await?;
        let existing = self
            .repository
            .session(&session)
            .await?
            .filter(|s| s["owner_scope"] == owner)
            .ok_or_else(|| error(404, "not_found"))?;
        self.reference_support(&body, &existing)?;
        if Self::has_reference(&body, &existing) {
            self.reference(user, &body, &existing, &owner).await?;
        }
        let reservation = self
            .repository
            .reserve(self.reservation(
                user,
                &body,
                &owner,
                &task,
                &turn,
                &session,
                "pending",
                &request_hash(&body)?,
            ))
            .await?;
        match string(&reservation, "state") {
            "conflict" => Err(error(409, "idempotency_conflict")),
            "existing" => {
                let old = self
                    .repository
                    .task(string(&reservation, "task_id"))
                    .await?
                    .ok_or_else(|| error(409, "duplicate_request_in_flight"))?;
                Ok(
                    json!({"task_id":reservation["task_id"],"status":old.get("status").cloned().unwrap_or(json!("pending")),"message":"Image generation task already submitted"}),
                )
            }
            "reserved" => Ok(
                json!({"task_id":task,"status":"pending","message":"Image generation task submitted"}),
            ),
            _ => Err(error(503, "storage_unavailable")),
        }
    }
    pub async fn generate(
        &self,
        user: &ContentIdentity,
        body: Value,
        claimed: Option<Value>,
    ) -> Result<Value, ContentError> {
        let owner = claimed
            .as_ref()
            .map(|r| string(r, "owner_scope").to_owned())
            .unwrap_or(owner_scope(user)?);
        let task = claimed
            .as_ref()
            .map(|r| string(r, "task_id").to_owned())
            .unwrap_or_else(|| format!("imt_{}", &uuid::Uuid::new_v4().simple().to_string()[..20]));
        let turn = claimed
            .as_ref()
            .map(|r| string(r, "turn_id").to_owned())
            .unwrap_or_else(|| format!("itn_{}", &uuid::Uuid::new_v4().simple().to_string()[..16]));
        let session = claimed
            .as_ref()
            .map(|r| string(r, "session_id").to_owned())
            .or_else(|| {
                body.get("session_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| format!("img_{}", uuid::Uuid::new_v4().simple()));
        let existing = self
            .repository
            .session(&session)
            .await?
            .unwrap_or(Value::Null);
        if !existing.is_null() && existing["owner_scope"] != owner {
            return Err(error(404, "not_found"));
        }
        self.reference_support(&body, &existing)?;
        let variants = body["return_variants"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if variants
            .iter()
            .any(|v| !matches!(v.as_str(), Some("raw" | "display" | "thumbnail")))
        {
            return Err(error(422, "invalid_variants"));
        }
        let hash = request_hash(&body)?;
        if claimed.is_none() {
            let reservation = self
                .repository
                .reserve(self.reservation(
                    user, &body, &owner, &task, &turn, &session, "running", &hash,
                ))
                .await?;
            match string(&reservation, "state") {
                "conflict" => return Err(error(409, "idempotency_conflict")),
                "existing" => {
                    if let Some(old) = self
                        .repository
                        .task(string(&reservation, "task_id"))
                        .await?
                    {
                        if old["result"].is_object() {
                            let mut result = old["result"].clone();
                            result["idempotent_replay"] = json!(true);
                            return Ok(result);
                        }
                    }
                    return Err(error(409, "duplicate_request_in_flight"));
                }
                "reserved" => (),
                _ => return Err(error(503, "storage_unavailable")),
            }
        }
        self.repository
            .upsert_session(user, &session, &owner)
            .await?;
        let existing = self
            .repository
            .session(&session)
            .await?
            .filter(|s| s["owner_scope"] == owner);
        let Some(existing) = existing else {
            self.repository.update_task(&task,json!({"status":"failed","progress":100,"error":"image session not found","error_code":"not_found"})).await?;
            return Err(error(404, "not_found"));
        };
        let parent = body
            .get("parent_artifact_id")
            .filter(|v| !v.is_null() && *v != "")
            .cloned()
            .unwrap_or(existing["latest_artifact_id"].clone());
        let reference = match self.reference(user, &body, &existing, &owner).await {
            Ok(reference) => reference,
            Err(e) => {
                let code = if e.status == 404 {
                    "reference_not_found"
                } else {
                    "invalid_reference"
                };
                self.repository.update_task(&task,json!({"status":"failed","progress":100,"error":if e.status==404{"reference not found"}else{"invalid reference"},"error_code":code})).await?;
                return Err(error(e.status, code));
            }
        };
        if !body["expected_parent_artifact_id"].is_null()
            && existing["latest_artifact_id"] != body["expected_parent_artifact_id"]
        {
            self.repository.update_task(&task,json!({"status":"failed","progress":100,"error":"session latest artifact changed","error_code":"latest_artifact_conflict"})).await?;
            return Err(error(409, "latest_artifact_conflict"));
        }
        self.repository.insert_turn(json!({"turn_id":turn,"session_id":session,"owner_scope":owner,"task_id":task,"prompt":body["prompt"],"model_id":self.provider_model.clone().unwrap_or_else(||string(&body,"model_id").into()),"style":body["style"],"add_watermark":body["add_watermark"],"parent_artifact_id":parent,"output_artifact_id":null,"status":"running","error":null,"error_code":null,"client_request_id":body["client_request_id"],"request_hash":hash})).await?;
        let started = Instant::now();
        let mut succeeded = false;
        let mut unknown = false;
        let operation=async{let mut args=json!({"prompt":body["prompt"],"n":body["n"],"size":body.get("size").filter(|v|!v.is_null()&&*v!="").cloned().unwrap_or(json!("1536*1536")),"style":body["style"],"negative_prompt":""});if let Some((bytes,mime))=reference{args["reference_image_base64"]=json!(STANDARD.encode(bytes));args["reference_mime"]=json!(mime);}let result=self.provider.generate(args).await?;unknown=result["outcome_unknown"]==true;if unknown{return Err(error(502,"outcome_unknown"))}let images=result["images"].as_array().filter(|a|!a.is_empty());if result["success"]!=true||images.is_none(){return Err(error(502,"provider_failed"))}succeeded=true;let(mut raws,mut displays,mut thumbnails)=(Vec::new(),Vec::new(),Vec::new());for(index,item)in images.unwrap().iter().take(body["n"].as_u64().unwrap_or(1)as usize).enumerate(){let index=index+1;let(data,mime)=decode_image(item,20*1024*1024)?;let ext=mime.trim_start_matches("image/").replace("jpeg","jpg");let raw=self.persist_variant(user,&body,&session,&turn,&owner,&parent,&result,"raw",index,index,&ext,"image_generation",data.clone()).await?;let raw_id=json!(raw.artifact_id);raws.push(raw);if body["add_watermark"]==true{let(watermark,mime)=self.transforms.watermark(data.clone()).await?;let fmt=if mime=="image/png"{"png"}else{&ext};displays.push(self.persist_variant(user,&body,&session,&turn,&owner,&raw_id,&result,"display",index,index+100,fmt,"image_generation_watermarked",watermark).await?);}if variants.iter().any(|v|v=="thumbnail"){if let Some(thumbnail)=self.transforms.thumbnail(data).await.unwrap_or(None){thumbnails.push(self.persist_variant(user,&body,&session,&turn,&owner,&raw_id,&result,"thumbnail",index,index+200,"png","image_generation_thumbnail",thumbnail).await?);}}}
        let raw=raws.first().ok_or_else(||error(502,"provider_failed"))?.artifact_id.clone();let expected=body.get("expected_parent_artifact_id").and_then(Value::as_str).filter(|s|!s.is_empty()).map(str::to_owned).or_else(||parent.as_str().map(str::to_owned));let advanced=self.repository.advance_latest(&session,expected,&raw).await?;let selected=if displays.is_empty(){&raws}else{&displays};let mut urls=vec![];for a in selected{let url=self.artifacts.variant_url(&a.artifact_id,"display",3600,Some(&owner),Some(&user.tenant_id),Some(&user.user_id)).await?.map(|v|v.0).unwrap_or_else(||format!("/api/v1/assistant/artifacts/{}/download",a.artifact_id));urls.push(json!({"url":url,"artifact_id":a.artifact_id}));}let mut requested=json!({});for variant in &variants{let name=variant.as_str().unwrap();let selected=match name{"raw"=>&raws,"display"=>if displays.is_empty(){&raws}else{&displays},_=>&thumbnails};if let Some(a)=selected.first(){if let Some((url,_))=self.artifacts.variant_url(&a.artifact_id,name,3600,Some(&owner),Some(&user.tenant_id),Some(&user.user_id)).await?{requested[name]=json!(url)}}}let response=json!({"success":true,"images":urls,"provider":result["provider"],"effective_model_id":self.provider_model,"duration_ms":started.elapsed().as_secs_f64()*1000.0,"session_id":session,"turn_id":turn,"parent_artifact_id":parent,"output_artifact_id":raw,"client_request_id":body["client_request_id"],"latest_advanced":advanced,"variants":if requested.as_object().unwrap().is_empty(){Value::Null}else{requested}});self.repository.update_turn(&turn,"completed",Some(raw.clone()),None,None).await?;self.repository.update_task(&task,json!({"status":"completed","progress":100,"provider":result["provider"],"result":response,"output_artifact_id":raw})).await?;Ok(response)}.await;
        match operation {
            Ok(result) => Ok(result),
            Err(_) => {
                let code = if succeeded || unknown {
                    "outcome_unknown"
                } else {
                    "provider_failed"
                };
                let status = if code == "outcome_unknown" {
                    "unknown"
                } else {
                    "failed"
                };
                let message = if code == "outcome_unknown" {
                    "image generation outcome unknown"
                } else {
                    "image generation failed"
                };
                self.repository
                    .update_turn(&turn, status, None, Some(message.into()), Some(code.into()))
                    .await?;
                self.repository
                    .update_task(
                        &task,
                        json!({"status":status,"progress":100,"error":message,"error_code":code}),
                    )
                    .await?;
                Err(ContentError::new(
                    502,
                    json!({"error_code":code,"task_id":task,"message":message}),
                ))
            }
        }
    }
    async fn persist_variant(
        &self,
        user: &ContentIdentity,
        body: &Value,
        session: &str,
        turn: &str,
        owner: &str,
        parent: &Value,
        result: &Value,
        variant: &str,
        index: usize,
        hash_index: usize,
        format: &str,
        source: &str,
        bytes: Vec<u8>,
    ) -> Result<Artifact, ContentError> {
        let hash = sha256(&bytes);
        let id = format!(
            "art_{}",
            &sha256(
                format!(
                    "{}\u{1f}{}\u{1f}{session}\u{1f}{turn}\u{1f}{hash_index}\u{1f}{hash}",
                    user.tenant_id, user.user_id
                )
                .as_bytes()
            )[..16]
        );
        let mut metadata = json!({"content_sha256":hash});
        if variant == "raw" {
            metadata["turn_id"] = json!(turn);
            metadata["owner_scope"] = json!(owner);
        }
        let filename = if variant == "raw" {
            format!("generated_{turn}_{index}.{format}")
        } else {
            format!("{variant}_{turn}_{index}.png")
        };
        let artifact: NewArtifact = serde_json::from_value(
            json!({"session_id":session,"tenant_id":user.tenant_id,"user_id":user.user_id,"type":"image","format":format,"title":format!("Generated: {}",string(body,"prompt").chars().take(60).collect::<String>()),"filename":filename,"source":source,"metadata":metadata,"variant":variant,"parent_artifact_id":parent,"turn_id":turn,"owner_scope":owner,"provider":result["provider"],"model_id":self.provider_model,"prompt":body["prompt"],"artifact_id":id}),
        )?;
        self.artifacts.create(artifact, bytes).await
    }
    pub async fn execute_claimed(&self, task: &str, body: Value) -> Result<Value, ContentError> {
        let row = self
            .repository
            .task(task)
            .await?
            .ok_or_else(|| error(409, "task_not_claimed"))?;
        let user = ContentIdentity {
            tenant_id: string(&row, "tenant_id").into(),
            user_id: string(&row, "user_id").into(),
            ..Default::default()
        };
        if user.tenant_id.is_empty() || user.user_id.is_empty() {
            return Err(error(409, "task_scope_invalid"));
        }
        let owner = string(&row, "owner_scope");
        if row["status"] != "claimed"
            || row["runtime_scope_version"] != 1
            || !(owner == user.user_id || owner == owner_scope(&user)?)
            || row["request_hash"] != request_hash(&body)?
        {
            return Err(error(409, "task_not_claimed"));
        }
        if !self.repository.dispatch_claim(&row).await? {
            return Err(error(409, "task_not_claimed"));
        }
        self.generate(&user, body, Some(row)).await
    }
    pub async fn task(&self, user: &ContentIdentity, id: &str) -> Result<Value, ContentError> {
        let row = match self.repository.task(id).await? {
            Some(r) => Some(r),
            None => self.repository.turn_by_task(id).await?,
        };
        let owner = owner_scope(user)?;
        row.filter(|r| {
            r["owner_scope"] == owner
                && (r.get("runtime_scope_version").is_none()
                    || (r["runtime_scope_version"] == 1
                        && r["tenant_id"] == user.tenant_id
                        && r["user_id"] == user.user_id))
        })
        .ok_or_else(|| error(404, "not_found"))
    }
    pub async fn session(
        &self,
        user: &ContentIdentity,
        id: &str,
        limit: i64,
        cursor: Option<&str>,
        include_urls: bool,
    ) -> Result<Value, ContentError> {
        let owner = owner_scope(user)?;
        let row = self
            .repository
            .session(id)
            .await?
            .filter(|r| r["owner_scope"] == owner)
            .ok_or_else(|| error(404, "not_found"))?;
        let (turns, next) = self.repository.turns(id, &owner, limit, cursor).await?;
        let mut public = vec![];
        for turn in turns {
            let mut visible = json!({});
            for key in [
                "turn_id",
                "session_id",
                "task_id",
                "prompt",
                "model_id",
                "style",
                "add_watermark",
                "parent_artifact_id",
                "output_artifact_id",
                "output_artifact_ids",
                "status",
                "error_code",
                "client_request_id",
                "created_at",
                "completed_at",
            ] {
                visible[key] = turn[key].clone()
            }
            visible["error"] = json!(public_image_error(
                string(&turn, "status"),
                string(&turn, "error_code")
            ));
            if include_urls && !string(&turn, "output_artifact_id").is_empty() {
                visible["output_url"] = json!(
                    self.artifacts
                        .variant_url(
                            string(&turn, "output_artifact_id"),
                            "display",
                            3600,
                            Some(&owner),
                            Some(&user.tenant_id),
                            Some(&user.user_id)
                        )
                        .await?
                        .map(|v| v.0)
                );
            }
            public.push(visible)
        }
        Ok(
            json!({"session_id":id,"latest_artifact_id":row["latest_artifact_id"],"locked_style":row["locked_style"],"created_at":row["created_at"],"updated_at":row["updated_at"],"turns":public,"next_cursor":next}),
        )
    }
    pub async fn blob_upload_url(
        &self,
        user: &ContentIdentity,
        body: &Value,
    ) -> Result<Value, ContentError> {
        let mime = string(body, "mime_type");
        if !["image/png", "image/jpeg", "image/gif", "image/webp"]
            .contains(&mime.to_lowercase().as_str())
            || body["byte_size"]
                .as_u64()
                .is_some_and(|n| n > 8 * 1024 * 1024)
            || (!string(body, "content_sha256").is_empty()
                && !hex_id(string(body, "content_sha256"), "", 64))
        {
            return Err(error(422, "invalid_upload"));
        }
        let owner = owner_scope(user)?;
        let id = format!("iblob_{}", &uuid::Uuid::new_v4().simple().to_string()[..24]);
        let key = ArtifactStorage::image_blob_key(&owner, &id, string(body, "filename"))?;
        let upload = self
            .artifacts
            .backend
            .upload_url(
                &key,
                mime,
                900,
                BTreeMap::from([
                    ("blob-id".into(), id.clone()),
                    ("owner-scope-sha256".into(), sha256(owner.as_bytes())),
                ]),
            )
            .await?
            .ok_or_else(|| error(503, "storage_unavailable"))?;
        if !self.repository.create_blob(json!({"blob_id":id,"owner_scope":owner,"content_sha256":body["content_sha256"],"byte_size":body["byte_size"],"mime_type":mime,"storage_key":key,"source":"user_upload","status":"pending"})).await?{return Err(error(503,"storage_unavailable"))}
        let headers = upload
            .get("headers")
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .unwrap_or(json!({"Content-Type":mime}));
        Ok(
            json!({"blob_id":id,"upload_url":upload.get("url").or_else(||upload.get("upload_url")).cloned().unwrap_or_default(),"method":"PUT","headers":headers,"fields":null,"storage_key":key,"expires_at":Utc::now()+chrono::Duration::seconds(900)}),
        )
    }
    pub async fn complete_blob(
        &self,
        user: &ContentIdentity,
        body: &Value,
    ) -> Result<Value, ContentError> {
        let owner = owner_scope(user)?;
        let id = string(body, "blob_id");
        if !string(body, "content_sha256").is_empty()
            && !hex_id(string(body, "content_sha256"), "", 64)
        {
            return Err(error(422, "invalid_upload"));
        }
        let row = self
            .repository
            .blob(id, Some(&owner))
            .await?
            .ok_or_else(|| error(404, "not_found"))?;
        if row["status"] == "ready" {
            for key in ["content_sha256", "byte_size", "mime_type"] {
                if !body[key].is_null() && body[key] != row[key] {
                    return Err(error(409, "blob_integrity_mismatch"));
                }
            }
            return Ok(blob_response(&row));
        }
        if row["status"] != "pending" {
            return Err(error(409, "blob_invalid"));
        }
        let key = string(&row, "storage_key");
        let info = self
            .artifacts
            .inspect_blob(key, 8 * 1024 * 1024)
            .await
            .map_err(|_| error(409, "blob_invalid"))?;
        let content = self
            .artifacts
            .read_blob(key, 8 * 1024 * 1024)
            .await
            .map_err(|_| error(409, "blob_invalid"))?;
        let mime = sniff_image(&content).ok_or_else(|| error(409, "blob_integrity_mismatch"))?;
        let digest = sha256(&content);
        for (key, actual) in [
            ("mime_type", json!(mime)),
            ("content_sha256", json!(digest)),
            ("byte_size", json!(content.len())),
        ] {
            let declared = body
                .get(key)
                .filter(|v| !v.is_null() && *v != "")
                .unwrap_or(&row[key]);
            if !declared.is_null() && declared != &actual {
                return Err(error(409, "blob_integrity_mismatch"));
            }
        }
        if info
            .and_then(|i| i.content_type)
            .is_some_and(|s| s.split(';').next().unwrap_or("").to_lowercase() != mime)
        {
            return Err(error(409, "blob_integrity_mismatch"));
        }
        let result = json!({"blob_id":id,"status":"ready","content_sha256":digest,"byte_size":content.len(),"mime_type":mime,"storage_key":key});
        if !self
            .repository
            .update_blob(id, &owner, result.clone())
            .await?
        {
            return Err(error(503, "storage_unavailable"));
        }
        Ok(result)
    }
    pub async fn fetch_blob(
        &self,
        user: &ContentIdentity,
        body: &Value,
    ) -> Result<Value, ContentError> {
        let fetched = self
            .fetcher
            .fetch(
                string(body, "url"),
                8 * 1024 * 1024,
                Duration::from_secs(30),
            )
            .await
            .map_err(|_| error(422, "invalid_image_url"))?;
        if !(200..300).contains(&fetched.status) {
            return Err(error(422, "invalid_image_url"));
        }
        let mime = sniff_image(&fetched.bytes).ok_or_else(|| error(422, "invalid_image"))?;
        let declared = string(body, "mime_type").to_lowercase();
        let response = fetched
            .content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if !declared.is_empty() && declared != mime || !response.is_empty() && response != mime {
            return Err(error(422, "invalid_image"));
        }
        let owner = owner_scope(user)?;
        let id = format!("iblob_{}", &uuid::Uuid::new_v4().simple().to_string()[..24]);
        let key = ArtifactStorage::image_blob_key(
            &owner,
            &id,
            &format!("reference.{}", mime.split('/').nth(1).unwrap()),
        )?;
        let digest = sha256(&fetched.bytes);
        let size = fetched.bytes.len();
        self.artifacts
            .store_blob(&key, fetched.bytes, mime, BTreeMap::new(), 8 * 1024 * 1024)
            .await?;
        let result = json!({"blob_id":id,"status":"ready","content_sha256":digest,"byte_size":size,"mime_type":mime,"storage_key":key});
        let mut row = result.clone();
        row["owner_scope"] = json!(owner);
        row["source"] = json!("remote_fetch");
        if !self.repository.create_blob(row).await? {
            return Err(error(503, "storage_unavailable"));
        }
        Ok(result)
    }
}
fn blob_response(row: &Value) -> Value {
    json!({"blob_id":row["blob_id"],"status":"ready","content_sha256":row["content_sha256"],"byte_size":row["byte_size"],"mime_type":row["mime_type"],"storage_key":row["storage_key"]})
}
pub struct ImageTaskWorker {
    pub service: Arc<ImageService>,
    stop: AtomicBool,
    running: tokio::sync::Mutex<()>,
}
impl ImageTaskWorker {
    pub fn new(service: Arc<ImageService>) -> Self {
        Self {
            service,
            stop: AtomicBool::new(false),
            running: tokio::sync::Mutex::new(()),
        }
    }
    pub(crate) fn prepare_start(&self) {
        self.stop.store(false, Ordering::Release);
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release)
    }
    pub async fn drain_and_wait(&self) {
        self.stop();
        let _guard = self.running.lock().await;
    }
    pub async fn run_once(&self) -> Result<usize, ContentError> {
        ai_platform_gateway_foundation::worker_lease::with_effect("image_task_worker", self.run_once_owned())
            .await.map_err(|_| error(503, "worker_fence_rejected"))?
    }
    async fn run_once_owned(&self) -> Result<usize, ContentError> {
        if self.stop.load(Ordering::Acquire) {
            return Ok(0);
        }
        let Ok(_guard) = self.running.try_lock() else {
            return Ok(0);
        };
        let rows = self.service.repository.claim_pending(1, 300).await?;
        let Some(task) = rows.first() else {
            return Ok(0);
        };
        let id = string(task, "task_id");
        let raw = if let Some(s) = task["request_payload"].as_str() {
            serde_json::from_str(s).map_err(ContentError::from)
        } else {
            Ok(task["request_payload"].clone())
        };
        let operation = async {
            let body = normalize_request(&raw?, true)?;
            self.service.execute_claimed(id, body).await
        }
        .await;
        if let Err(e) = operation {
            self.service
                .repository
                .mark_unknown(id, "worker execution failed")
                .await?;
            return Err(e);
        }
        Ok(1)
    }
    pub async fn run(&self, poll: Duration) {
        while !self.stop.load(Ordering::Acquire) {
            if self.run_once().await.unwrap_or(0) == 0 {
                tokio::time::sleep(poll.max(Duration::from_millis(50))).await;
            }
        }
        self.drain_and_wait().await;
    }
}
pub fn router(service: Arc<ImageService>) -> Router {
    Router::new()
        .route("/assistant/generate-image", post(generate))
        .route("/assistant/generate-image-async", post(submit))
        .route("/assistant/image-task/{id}", get(task))
        .route("/assistant/image-sessions/{id}", get(session))
        .route(
            "/assistant/artifacts/{artifact_id}/download-url",
            get(download_url),
        )
        .route("/assistant/image-blobs/upload-url", post(blob_upload))
        .route("/assistant/image-blobs/complete", post(blob_complete))
        .route("/assistant/image-blobs/fetch-url", post(blob_fetch))
        .with_state(service)
}
async fn image_actor(
    s: &ImageService,
    h: &HeaderMap,
    rate: bool,
) -> Result<ContentIdentity, ContentError> {
    let u = s.access.resolve(h).await?;
    if rate {
        s.access.rate_limit(&u, "image_generate", None).await?
    }
    Ok(u)
}
async fn generate(
    State(s): State<Arc<ImageService>>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ContentError> {
    let u = image_actor(&s, &h, true).await?;
    Ok(Json(
        s.generate(&u, normalize_request(&body, false)?, None)
            .await?,
    ))
}
async fn submit(
    State(s): State<Arc<ImageService>>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ContentError> {
    let u = image_actor(&s, &h, true).await?;
    Ok(Json(s.submit(&u, normalize_request(&body, true)?).await?))
}
async fn task(
    State(s): State<Arc<ImageService>>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = image_actor(&s, &h, false).await?;
    let row = s.task(&u, &id).await?;
    let result = &row["result"];
    let mut response = json!({"task_id":id,"status":row.get("status").cloned().unwrap_or(json!("pending")),"progress":row.get("progress").cloned().unwrap_or(json!(0)),"prompt":row.get("prompt").cloned().unwrap_or(json!("")),"model_id":row.get("model_id").cloned().unwrap_or(json!("")),"provider":row["provider"],"effective_model_id":result["effective_model_id"],"images":result.get("images").cloned().unwrap_or(json!([])),"duration_ms":result["duration_ms"],"error":public_image_error(string(&row,"status"),string(&row,"error_code")),"latest_advanced":result["latest_advanced"]});
    for k in [
        "error_code",
        "created_at",
        "completed_at",
        "turn_id",
        "session_id",
        "parent_artifact_id",
        "output_artifact_id",
        "client_request_id",
    ] {
        response[k] = row[k].clone()
    }
    Ok(Json(response))
}
async fn session(
    State(s): State<Arc<ImageService>>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    h: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = image_actor(&s, &h, false).await?;
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(ContentError::new(422, "Invalid limit"));
    }
    Ok(Json(
        s.session(
            &u,
            &id,
            limit,
            q.get("cursor").map(String::as_str),
            q.get("include_urls")
                .is_some_and(|s| s == "true" || s == "1"),
        )
        .await?,
    ))
}
async fn download_url(
    State(s): State<Arc<ImageService>>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    h: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = image_actor(&s, &h, false).await?;
    let owner = owner_scope(&u)?;
    let variant = q.get("variant").map(String::as_str).unwrap_or("display");
    let expires = q
        .get("expires_in")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(3600);
    if !["raw", "display", "thumbnail"].contains(&variant) || !(60..=3600).contains(&expires) {
        return Err(ContentError::new(422, "Invalid download parameters"));
    }
    let (url, actual) = s
        .artifacts
        .variant_url(
            &id,
            variant,
            expires,
            Some(&owner),
            Some(&u.tenant_id),
            Some(&u.user_id),
        )
        .await?
        .ok_or_else(|| error(404, "not_found"))?;
    Ok(Json(
        json!({"artifact_id":id,"variant":actual,"url":url,"expires_at":Utc::now()+chrono::Duration::seconds(expires as i64)}),
    ))
}
async fn blob_upload(
    State(s): State<Arc<ImageService>>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ContentError> {
    let u = image_actor(&s, &h, true).await?;
    Ok(Json(s.blob_upload_url(&u, &body).await?))
}
async fn blob_complete(
    State(s): State<Arc<ImageService>>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ContentError> {
    let u = image_actor(&s, &h, true).await?;
    Ok(Json(s.complete_blob(&u, &body).await?))
}
async fn blob_fetch(
    State(s): State<Arc<ImageService>>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ContentError> {
    let u = image_actor(&s, &h, true).await?;
    Ok(Json(s.fetch_blob(&u, &body).await?))
}
