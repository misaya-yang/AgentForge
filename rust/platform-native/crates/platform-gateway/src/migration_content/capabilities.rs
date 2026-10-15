//! Scope-bound content brokers. Sandbox and provider execution remain remote.
use super::artifacts::{ArtifactStorage, NewArtifact};
use super::attachments::{self, PdfAttachmentParser};
use super::{ContentDatabase, ContentError, ContentFuture, hex_id, sha256, string};
use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{collections::BTreeMap, sync::Arc};
const RECEIPT: &str = "ai-platform/durable-capability-receipt/v1";
const PENDING: &str = "ai-platform/capability-broker-dispatch/v1";
pub fn canonical_json(value: &Value) -> Result<Vec<u8>, ContentError> {
    Ok(super::python_json(value, false, false).into_bytes())
}
pub fn constant_time_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |n, (a, b)| n | (a ^ b)) == 0
}
#[derive(Clone, Debug)]
pub struct CapabilityScope {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub execution_id: String,
    pub run_id: String,
    pub tool_call_id: String,
    pub arguments_hash: String,
}
#[derive(Clone)]
pub struct CapabilityAuthorization {
    internal_token: Arc<str>,
    proof_secret: Arc<[u8]>,
}
impl CapabilityAuthorization {
    pub fn new(internal_token: String, proof_secret: Vec<u8>) -> Self {
        Self {
            internal_token: internal_token.into(),
            proof_secret: proof_secret.into(),
        }
    }
    pub fn peer(&self, headers: &HeaderMap) -> Result<(), ContentError> {
        let got = headers
            .get("x-ai-platform-internal-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if self.internal_token.is_empty()
            || got.is_empty()
            || !constant_time_equal(&self.internal_token, got)
        {
            Err(ContentError::new(401, "internal authorization failed"))
        } else {
            Ok(())
        }
    }
    pub fn authorize(
        &self,
        headers: &HeaderMap,
        path: &str,
        body: &Value,
        now: i64,
    ) -> Result<CapabilityScope, ContentError> {
        self.peer(headers)?;
        let read = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned()
        };
        let s = CapabilityScope {
            tenant_id: read("x-ai-tenant-id"),
            user_id: read("x-ai-user-id"),
            session_id: read("x-ai-session-id"),
            execution_id: read("x-ai-execution-id"),
            run_id: read("x-ai-run-id"),
            tool_call_id: read("x-ai-tool-call-id"),
            arguments_hash: read("x-ai-arguments-hash"),
        };
        for (label, v) in [
            ("tenant", &s.tenant_id),
            ("user", &s.user_id),
            ("session", &s.session_id),
        ] {
            if v.is_empty() || v.chars().count() > 255 || v.chars().any(|c| (c as u32) < 32) {
                return Err(ContentError::new(403, format!("{label} is invalid")));
            }
        }
        let proof = read("x-ai-capability-proof");
        if s.execution_id.is_empty() || s.run_id.is_empty() || proof.is_empty() {
            return Err(ContentError::new(401, "capability proof required"));
        }
        verify_capability_proof(&self.proof_secret, &proof, "POST", path, body, &s, now)?;
        Ok(s)
    }
}
pub fn verify_capability_proof(
    secret: &[u8],
    header: &str,
    method: &str,
    path: &str,
    body: &Value,
    scope: &CapabilityScope,
    now: i64,
) -> Result<Value, ContentError> {
    let invalid = || ContentError::new(401, "capability proof invalid");
    if secret.len() < 32 {
        return Err(invalid());
    }
    let encoded = header
        .strip_prefix("v1.")
        .filter(|s| !s.is_empty() && s.len() <= 4096)
        .ok_or_else(invalid)?;
    let raw = URL_SAFE_NO_PAD
        .decode(encoded.trim_end_matches('='))
        .map_err(|_| invalid())?;
    let mut envelope: Value = serde_json::from_slice(&raw).map_err(|_| invalid())?;
    let signature = envelope
        .as_object_mut()
        .ok_or_else(invalid)?
        .remove("signature")
        .and_then(|v| v.as_str().map(str::to_owned))
        .filter(|s| s.len() == 64)
        .ok_or_else(invalid)?;
    let expected_fields = [
        "schema_version",
        "method",
        "path",
        "body_sha256",
        "tenant_id",
        "user_id",
        "session_id",
        "execution_id",
        "run_id",
        "expires_at",
        "nonce",
    ];
    let map = envelope.as_object().ok_or_else(invalid)?;
    if map.len() != expected_fields.len()
        || !expected_fields.iter().all(|k| map.contains_key(*k))
        || envelope["schema_version"] != "ai-platform-capability-proof/v1"
    {
        return Err(invalid());
    }
    for key in [
        "method",
        "path",
        "body_sha256",
        "tenant_id",
        "user_id",
        "session_id",
        "execution_id",
        "run_id",
        "nonce",
    ] {
        if string(&envelope, key).is_empty() {
            return Err(invalid());
        }
    }
    for key in [
        "tenant_id",
        "user_id",
        "session_id",
        "execution_id",
        "run_id",
        "nonce",
    ] {
        let v = string(&envelope, key);
        if v.chars().count() > 255 || v.chars().any(|c| (c as u32) < 32) {
            return Err(invalid());
        }
    }
    if string(&envelope, "nonce").chars().count() > 128
        || string(&envelope, "path").chars().count() > 2048
        || string(&envelope, "path").chars().any(|c| (c as u32) < 32)
        || !hex_id(string(&envelope, "body_sha256"), "", 64)
    {
        return Err(invalid());
    }
    let expiry = envelope["expires_at"].as_i64().ok_or_else(invalid)?;
    if expiry <= 0 || expiry <= now || expiry > now + 120 {
        return Err(invalid());
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).map_err(|_| invalid())?;
    mac.update(&canonical_json(&envelope)?);
    let signed = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if !constant_time_equal(&signature, &signed) {
        return Err(invalid());
    }
    let hash = sha256(&canonical_json(body)?);
    let method = method.trim().to_uppercase();
    for (k, v) in [
        ("method", method.as_str()),
        ("path", path),
        ("body_sha256", hash.as_str()),
        ("tenant_id", scope.tenant_id.as_str()),
        ("user_id", scope.user_id.as_str()),
        ("session_id", scope.session_id.as_str()),
        ("execution_id", scope.execution_id.as_str()),
        ("run_id", scope.run_id.as_str()),
    ] {
        if !constant_time_equal(string(&envelope, k), v) {
            return Err(invalid());
        }
    }
    Ok(envelope)
}
fn filename(name: &str) -> Result<(), ContentError> {
    if name.is_empty()
        || name.len() > 255
        || matches!(name, "." | "..")
        || name.contains(['/', '\\'])
        || name.chars().any(|c| (c as u32) < 32)
    {
        Err(ContentError::new(424, "artifact filename is invalid"))
    } else {
        Ok(())
    }
}
fn valid_mime(m: &str) -> bool {
    m.split_once('/').is_some_and(|(a, b)| {
        [a, b].iter().all(|s| {
            !s.is_empty()
                && s.as_bytes()[0].is_ascii_alphanumeric()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&b))
        })
    })
}
fn decode_content(body: &Value, max: usize) -> Result<Vec<u8>, ContentError> {
    let bytes = STANDARD
        .decode(string(body, "content_base64"))
        .map_err(|_| ContentError::new(422, "content_base64 invalid"))?;
    if bytes.is_empty()
        || bytes.len() > max
        || body["size_bytes"].as_u64() != Some(bytes.len() as u64)
        || !hex_id(string(body, "sha256"), "", 64)
        || !constant_time_equal(&sha256(&bytes), string(body, "sha256"))
    {
        return Err(ContentError::new(422, "content size or hash mismatch"));
    }
    Ok(bytes)
}
fn validate_write(
    scope: &CapabilityScope,
    body: &Value,
    office: bool,
) -> Result<Vec<u8>, ContentError> {
    if uuid::Uuid::parse_str(&scope.execution_id).is_err()
        || uuid::Uuid::parse_str(&scope.run_id).is_err()
    {
        return Err(ContentError::new(401, "capability scope invalid"));
    }
    if !hex_id(&scope.arguments_hash, "sha256:", 64) {
        return Err(ContentError::new(401, "arguments hash required"));
    }
    let fields = if office {
        &[
            "tool_call_id",
            "arguments_hash",
            "artifact_type",
            "format",
            "title",
            "filename",
            "mime_type",
            "sha256",
            "size_bytes",
            "content_base64",
            "metadata",
        ][..]
    } else {
        &[
            "tool_call_id",
            "arguments_hash",
            "filename",
            "mime_type",
            "sha256",
            "size_bytes",
            "content_base64",
        ][..]
    };
    if !body.is_object()
        || body
            .as_object()
            .unwrap()
            .keys()
            .any(|k| !fields.contains(&k.as_str()))
    {
        return Err(ContentError::new(422, "invalid artifact"));
    }
    for key in ["tool_call_id", "filename"] {
        let v = string(body, key);
        if v.is_empty()
            || v.chars().any(|c| (c as u32) < 32)
            || v.chars().count() > if key == "tool_call_id" { 160 } else { 255 }
        {
            return Err(ContentError::new(422, "invalid artifact field"));
        }
    }
    if !office {
        filename(string(body, "filename"))?;
        filename(string(body, "tool_call_id"))?;
    }
    if !body["mime_type"].is_null() && !valid_mime(string(body, "mime_type")) {
        return Err(ContentError::new(422, "mime type invalid"));
    }
    if office {
        for key in ["artifact_type", "format", "title"] {
            let v = string(body, key);
            if v.is_empty()
                || v.chars().count() > if key == "title" { 255 } else { 32 }
                || v.chars().any(|c| (c as u32) < 32)
            {
                return Err(ContentError::new(422, "invalid office artifact"));
            }
        }
        let f = string(body, "format");
        if !f.as_bytes()[0].is_ascii_lowercase() && !f.as_bytes()[0].is_ascii_digit()
            || !f
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        {
            return Err(ContentError::new(422, "invalid office artifact"));
        }
    }
    if string(body, "tool_call_id") != scope.tool_call_id
        || string(body, "arguments_hash") != scope.arguments_hash
    {
        return Err(ContentError::new(403, "capability scope mismatch"));
    }
    decode_content(
        body,
        if office {
            32 * 1024 * 1024
        } else {
            24 * 1024 * 1024
        },
    )
}
fn active(row: &Value, scope: &CapabilityScope, capability: &str) -> Result<(), ContentError> {
    if string(row, "tool_call_id") != scope.tool_call_id
        || !constant_time_equal(
            string(row, "arguments_sha256").trim(),
            scope.arguments_hash.trim_start_matches("sha256:"),
        )
    {
        return Err(ContentError::new(403, "capability scope mismatch"));
    }
    if row["capability_id"] != capability {
        return Err(ContentError::new(403, "capability not authorized"));
    }
    if row["effect"] != "write"
        || row["approval_status"] != "consumed"
        || !matches!(string(row, "status"), "dispatched" | "running")
    {
        return Err(ContentError::new(403, "capability execution not active"));
    }
    Ok(())
}
async fn execution_row(
    c: &mut sqlx::PgConnection,
    s: &CapabilityScope,
) -> Result<Option<Value>, sqlx::Error> {
    sqlx::query_scalar("SELECT to_jsonb(e) FROM assistant_capability_executions e WHERE execution_id=$1::text::uuid AND tenant_id=$2 AND user_id=$3 AND session_id=$4 AND run_id=$5::text::uuid FOR UPDATE").bind(&s.execution_id).bind(&s.tenant_id).bind(&s.user_id).bind(&s.session_id).bind(&s.run_id).fetch_optional(c).await
}
#[derive(Clone)]
pub struct ContentCapabilityBroker {
    pub database: ContentDatabase,
    pub artifacts: Arc<ArtifactStorage>,
    pub authorization: CapabilityAuthorization,
    pub image_provider: Arc<dyn ImageProvider>,
    pub python: Arc<PythonCodeCapability>,
    pub pdf: Arc<dyn PdfAttachmentParser>,
}
impl ContentCapabilityBroker {
    pub async fn put_artifact(
        &self,
        s: CapabilityScope,
        body: Value,
        office: bool,
    ) -> Result<Value, ContentError> {
        persist_capability_artifact(&self.database, &self.artifacts, s, body, office).await
    }
    pub async fn generate_image(
        &self,
        s: CapabilityScope,
        body: Value,
    ) -> Result<Value, ContentError> {
        let args = validate_image_arguments(&body)?;
        let mut s = s;
        s.arguments_hash = string(&body, "arguments_hash").into();
        if uuid::Uuid::parse_str(&s.execution_id).is_err()
            || uuid::Uuid::parse_str(&s.run_id).is_err()
        {
            return Err(ContentError::new(403, "capability scope invalid"));
        }
        let owned = s.clone();
        let prior=self.database.transaction(move|c|Box::pin(async move{let Some(row)=execution_row(c,&owned).await?else{return Ok(Err(ContentError::new(403,"capability execution not active")))};if active(&row,&owned,"generate_image").is_err(){return Ok(Err(ContentError::new(403,"capability execution not active")))}if let Some(result)=durable_image_result(&row["result_summary"]){return Ok(Ok(Some(result)))}if !row["result_summary"].is_null(){return Ok(Err(ContentError::new(502,"image generation outcome unknown")))}let changed=sqlx::query("UPDATE assistant_capability_executions SET result_summary=$2,updated_at=NOW() WHERE execution_id=$1::text::uuid").bind(&owned.execution_id).bind(json!({"schema_version":PENDING,"capability_id":"generate_image"})).execute(c).await?.rows_affected();Ok(if changed==1{Ok(None)}else{Err(ContentError::new(502,"image generation outcome unknown"))})})).await?;
        if let Some(prior) = prior {
            return Ok(gateway_image_response(&prior));
        }
        let result = self
            .image_provider
            .generate(args.clone())
            .await
            .map_err(|_| ContentError::new(502, "image provider unavailable"))?;
        if result["success"] != true {
            if result["outcome_unknown"] == true {
                return Err(ContentError::new(502, "image generation outcome unknown"));
            }
            self.finish_image_claim(&s.execution_id, None).await?;
            return Err(ContentError::new(422, "image generation failed"));
        }
        let images: Vec<_> = result["images"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|v| v.is_object())
                    .take(4)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if images.is_empty() {
            return Err(ContentError::new(502, "image provider returned no images"));
        }
        let mut artifacts = vec![];
        for (index, image) in images.into_iter().enumerate() {
            let (content, mime) = decode_image(&image, 20 * 1024 * 1024)
                .map_err(|_| ContentError::new(502, "image artifact persistence failed"))?;
            let index = index + 1;
            let format = mime
                .split('/')
                .nth(1)
                .unwrap_or("png")
                .split(';')
                .next()
                .unwrap_or("png")
                .to_lowercase()
                .replace("jpeg", "jpg");
            let id = format!(
                "art_{}",
                &sha256(
                    format!(
                        "image\0{}\0{}\0{}\0{}\0{index}",
                        s.tenant_id, s.user_id, s.session_id, s.execution_id
                    )
                    .as_bytes()
                )[..16]
            );
            let provider = result
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("configured");
            let n: NewArtifact = serde_json::from_value(
                json!({"session_id":s.session_id,"tenant_id":s.tenant_id,"user_id":s.user_id,"type":"image","format":format,"title":format!("Generated: {}",string(&args,"prompt").chars().take(60).collect::<String>()),"filename":format!("generated_{}_{index}.{format}",s.execution_id.chars().take(16).collect::<String>()),"source":"image_generation","metadata":{"execution_id":s.execution_id,"provider":provider,"content_sha256":sha256(&content),"artifact_index":index},"provider":provider,"prompt":args["prompt"],"artifact_id":id}),
            )?;
            let a = self
                .artifacts
                .create(n, content)
                .await
                .map_err(|_| ContentError::new(502, "image artifact persistence failed"))?;
            artifacts.push(json!({"artifact_id":a.artifact_id,"kind":"image","mime_type":mime,"filename":a.filename,"size_bytes":a.size_bytes}));
        }
        let result = json!({"receipt_id":format!("img_{}",&sha256(s.execution_id.as_bytes())[..32]),"capability_id":"generate_image","external_id":null,"external_url":null,"artifacts":artifacts});
        self.finish_image_claim(&s.execution_id, Some(result.clone()))
            .await?;
        Ok(gateway_image_response(&result))
    }
    async fn finish_image_claim(
        &self,
        execution: &str,
        result: Option<Value>,
    ) -> Result<(), ContentError> {
        let execution = execution.to_owned();
        let receipt=result.map(|result|json!({"schema_version":RECEIPT,"capability_id":"generate_image","result":result}));
        let changed=self.database.run(move|c|Box::pin(async move{sqlx::query("UPDATE assistant_capability_executions SET result_summary=$2,updated_at=NOW() WHERE execution_id=$1::text::uuid AND result_summary->>'schema_version'=$3").bind(execution).bind(receipt).bind(PENDING).execute(c).await.map(|r|r.rows_affected())})).await?;
        if changed != 1 {
            Err(ContentError::new(502, "image generation outcome unknown"))
        } else {
            Ok(())
        }
    }
}
pub async fn persist_capability_artifact(
    database: &ContentDatabase,
    artifacts: &Arc<ArtifactStorage>,
    s: CapabilityScope,
    body: Value,
    office: bool,
) -> Result<Value, ContentError> {
    let bytes = validate_write(&s, &body, office)?;
    let storage = artifacts.clone();
    let result=database.transaction(move|c|Box::pin(async move{let Some(row)=execution_row(c,&s).await?else{return Ok(Err(ContentError::new(403,"capability scope mismatch")))};if string(&row,"tool_call_id")!=s.tool_call_id||string(&row,"arguments_sha256").trim()!=s.arguments_hash.trim_start_matches("sha256:"){return Ok(Err(ContentError::new(403,"capability scope mismatch")))}
        if office{if let Some(receipt)=office_receipt(&row["result_summary"]){if receipt["sha256"]!=body["sha256"]||receipt["size_bytes"]!=body["size_bytes"]{return Ok(Err(ContentError::new(409,"artifact idempotency conflict")))}return Ok(Ok(receipt))}}
        if let Err(e)=active(&row,&s,if office{"mcp_docgen__generate_document"}else{"execute_python_code"}){return Ok(Err(e))}
        let identity=if office{format!("office\0{}\0{}\0{}\0{}\0{}\0{}",s.tenant_id,s.user_id,s.session_id,s.execution_id,s.tool_call_id,s.arguments_hash)}else{format!("python-output\0{}\0{}\0{}\0{}\0{}",s.execution_id,s.tool_call_id,s.arguments_hash,string(&body,"filename"),string(&body,"sha256"))};let id=format!("art_{}",&sha256(identity.as_bytes())[..16]);
        if !office{let prior:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(a) FROM artifacts a WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3 AND source='code_execution' AND metadata->>'execution_id'=$4 FOR UPDATE").bind(&s.tenant_id).bind(&s.user_id).bind(&s.session_id).bind(&s.execution_id).fetch_all(&mut *c).await?;if prior.iter().any(|a|a["filename"]==body["filename"]&&a["artifact_id"]!=id){return Ok(Err(ContentError::new(409,"artifact filename conflict")))}let replay=prior.iter().any(|a|a["artifact_id"]==id);if !replay&&(prior.len()>=64||prior.iter().filter_map(|a|a["size_bytes"].as_u64()).sum::<u64>()+bytes.len()as u64>24*1024*1024){return Ok(Err(ContentError::new(413,"artifact output limit exceeded")))}}
        let mut metadata=if office&&body["metadata"].is_object(){body["metadata"].clone()}else{json!({})};metadata["schema_version"]=json!(if office{"ai-platform/office-artifact/v1"}else{"ai-platform/python-code-artifact/v1"});metadata["execution_id"]=json!(s.execution_id);metadata["tool_call_id"]=json!(s.tool_call_id);metadata["arguments_hash"]=json!(s.arguments_hash);metadata["content_sha256"]=body["sha256"].clone();if !office{metadata["run_id"]=json!(s.run_id);metadata["requested_mime_type"]=body["mime_type"].clone();}
        let format=if office{string(&body,"format")}else{string(&body,"filename").rsplit('.').next().filter(|s|!s.is_empty()).unwrap_or("bin")}.to_lowercase();let new:NewArtifact=match serde_json::from_value(json!({"session_id":s.session_id,"tenant_id":s.tenant_id,"user_id":s.user_id,"type":if office{string(&body,"artifact_type")}else{"code"},"format":format,"title":if office{string(&body,"title")}else{string(&body,"filename")},"filename":body["filename"],"source":if office{"ai"}else{"code_execution"},"metadata":metadata,"artifact_id":id})){Ok(n)=>n,Err(_)=>return Ok(Err(ContentError::new(422,"invalid artifact")))};let artifact=match storage.create(new,bytes).await{Ok(a)=>a,Err(e)=>return Ok(Err(e))};let path=format!("/api/v1/assistant/artifacts/{}/download",artifact.artifact_id);let mut response=json!({"artifact_id":artifact.artifact_id,"filename":artifact.filename,"mime_type":artifact.mime_type.unwrap_or_else(||string(&body,"mime_type").into()),"size_bytes":artifact.size_bytes,"sha256":body["sha256"]});response[if office{"download_path"}else{"download_url"}]=json!(path);
        if office{let receipt=json!({"schema_version":RECEIPT,"capability_id":"mcp_docgen__generate_document","result":{"artifact_id":response["artifact_id"],"download_url":path,"filename":response["filename"],"size_bytes":response["size_bytes"],"sha256":response["sha256"],"plan_outline":string(&body["metadata"],"plan_outline"),"critic_passed":false},"broker_response":response});sqlx::query("UPDATE assistant_capability_executions SET result_summary=$2,updated_at=NOW() WHERE execution_id=$1::text::uuid").bind(&s.execution_id).bind(receipt).execute(c).await?;}Ok(Ok(response))})).await;
    match result {
        Ok(v) => Ok(v),
        Err(e) if e.status != 503 => Err(e),
        Err(_) => Err(ContentError::new(
            502,
            "artifact persistence outcome unknown",
        )),
    }
}

fn office_receipt(v: &Value) -> Option<Value> {
    if v["schema_version"] != RECEIPT
        || v["capability_id"] != "mcp_docgen__generate_document"
        || !v["result"].is_object()
    {
        return None;
    }
    let a = &v["broker_response"];
    let keys = [
        "artifact_id",
        "download_path",
        "filename",
        "mime_type",
        "size_bytes",
        "sha256",
    ];
    if a.as_object()?.len() != keys.len()
        || !keys.iter().all(|k| a.get(*k).is_some())
        || keys
            .iter()
            .filter(|k| **k != "size_bytes")
            .any(|k| !a[*k].is_string())
        || a["size_bytes"].as_i64().is_none()
        || !hex_id(string(a, "sha256"), "", 64)
        || !string(a, "artifact_id").starts_with("art_")
        || !string(a, "download_path").starts_with("/api/v1/assistant/artifacts/")
        || !valid_mime(string(a, "mime_type"))
        || ["artifact_id", "download_path", "filename", "mime_type"]
            .iter()
            .any(|k| string(a, k).chars().any(|c| (c as u32) < 32))
    {
        None
    } else {
        Some(a.clone())
    }
}
pub trait ImageProvider: Send + Sync {
    fn generate<'a>(&'a self, arguments: Value) -> ContentFuture<'a, Value>;
}
pub fn decode_image(image: &Value, max: usize) -> Result<(Vec<u8>, String), ContentError> {
    let mime = image
        .get("mime_type")
        .and_then(Value::as_str)
        .unwrap_or("image/png");
    let bytes = STANDARD
        .decode(string(image, "content_base64"))
        .map_err(|_| ContentError::new(422, "image response invalid"))?;
    if bytes.is_empty()
        || bytes.len() > max
        || attachments::sniff_image(&bytes)
            != Some(if mime == "image/jpg" {
                "image/jpeg"
            } else {
                mime
            })
    {
        return Err(ContentError::new(422, "image response invalid"));
    }
    Ok((bytes, mime.into()))
}
fn validate_image_arguments(body: &Value) -> Result<Value, ContentError> {
    let invalid = || ContentError::new(422, "invalid image arguments");
    if body.as_object().is_none_or(|m| m.len() != 2)
        || !body["arguments"].is_object()
        || format!("sha256:{}", sha256(&canonical_json(&body["arguments"])?))
            != string(body, "arguments_hash")
    {
        return Err(invalid());
    }
    let mut args = body["arguments"].clone();
    if args
        .as_object()
        .unwrap()
        .keys()
        .any(|k| !["prompt", "negative_prompt", "size", "style", "n"].contains(&k.as_str()))
    {
        return Err(invalid());
    }
    for (key, default) in [
        ("negative_prompt", json!("")),
        ("size", json!("1536*1536")),
        ("style", json!("<auto>")),
        ("n", json!(1)),
    ] {
        if args.get(key).is_none() {
            args[key] = default;
        }
    }
    if string(&args, "prompt").is_empty()
        || string(&args, "prompt").chars().count() > 8000
        || string(&args, "negative_prompt").chars().count() > 4000
        || string(&args, "style").chars().count() > 64
        || !["1536*1536", "1024*1024", "720*1280", "1280*720"].contains(&string(&args, "size"))
        || !args["n"].as_i64().is_some_and(|n| (1..=4).contains(&n))
    {
        return Err(invalid());
    }
    Ok(args)
}
fn durable_image_result(v: &Value) -> Option<Value> {
    if v["schema_version"] != RECEIPT || v["capability_id"] != "generate_image" {
        return None;
    }
    let r = &v["result"];
    if r.as_object()?.len() != 5
        || r["capability_id"] != "generate_image"
        || string(r, "receipt_id").is_empty()
        || !r["external_id"].is_null()
        || !r["external_url"].is_null()
    {
        return None;
    }
    let artifacts = r["artifacts"].as_array()?;
    if !(1..=4).contains(&artifacts.len())
        || artifacts.iter().any(|a| {
            a.as_object().is_none_or(|m| m.len() != 5)
                || a["kind"] != "image"
                || !a["size_bytes"].as_i64().is_some_and(|n| n > 0)
                || ["artifact_id", "kind", "mime_type", "filename"]
                    .iter()
                    .any(|k| string(a, k).is_empty())
        })
    {
        return None;
    }
    Some(r.clone())
}
fn gateway_image_response(r: &Value) -> Value {
    json!({"receipt_id":r["receipt_id"],"external_id":"","external_url":"","artifacts":r["artifacts"]})
}

pub trait SandboxTransport: Send + Sync {
    fn execute<'a>(&'a self, payload: Value) -> ContentFuture<'a, Value>;
}
pub struct HttpSandboxTransport {
    client: reqwest::Client,
    url: reqwest::Url,
    token: String,
}
impl HttpSandboxTransport {
    pub fn new(client: reqwest::Client, base: &str, token: String) -> Result<Self, ContentError> {
        let mut url = reqwest::Url::parse(base)
            .map_err(|_| ContentError::new(400, "sandbox broker URL must be an http(s) origin"))?;
        if !["http", "https"].contains(&url.scheme())
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || token.len() < 32
        {
            return Err(ContentError::new(
                400,
                "invalid sandbox broker configuration",
            ));
        }
        url.set_path("/internal/v2/sandbox/python/execute");
        url.set_query(None);
        url.set_fragment(None);
        Ok(Self { client, url, token })
    }
}
impl SandboxTransport for HttpSandboxTransport {
    fn execute<'a>(&'a self, payload: Value) -> ContentFuture<'a, Value> {
        Box::pin(async move {
            let timeout = payload["limits"]["timeout_seconds"].as_u64().unwrap_or(30) + 10;
            let response = self
                .client
                .post(self.url.clone())
                .header("x-ai-platform-internal-token", &self.token)
                .timeout(std::time::Duration::from_secs(timeout))
                .json(&payload)
                .send()
                .await
                .map_err(|e| {
                    ContentError::new(
                        424,
                        if e.is_timeout() {
                            "sandbox outcome is unknown"
                        } else {
                            "sandbox is unavailable"
                        },
                    )
                })?;
            if response.status().is_server_error() {
                return Err(ContentError::new(424, "sandbox outcome is unknown"));
            }
            if response.status().is_client_error() {
                return Err(ContentError::new(424, "sandbox rejected execution"));
            }
            let result: Value = response
                .json()
                .await
                .map_err(|_| ContentError::new(424, "sandbox returned malformed result"))?;
            if !result.is_object() {
                return Err(ContentError::new(424, "sandbox returned malformed result"));
            }
            Ok(result)
        })
    }
}
pub trait CodeArtifactSink: Send + Sync {
    fn put_code_output<'a>(&'a self, scope: Value, artifact: Value) -> ContentFuture<'a, Value>;
}
/// Writes sandbox output through the same capability execution checks and
/// deterministic artifact persistence used by the private artifact endpoint.
pub struct StorageCodeArtifactSink {
    pub database: ContentDatabase,
    pub artifacts: Arc<ArtifactStorage>,
}
impl CodeArtifactSink for StorageCodeArtifactSink {
    fn put_code_output<'a>(
        &'a self,
        lease: Value,
        mut artifact: Value,
    ) -> ContentFuture<'a, Value> {
        Box::pin(async move {
            let execution = string(&lease, "execution_id").to_owned();
            let tenant = string(&lease, "tenant_id").to_owned();
            let user = string(&lease, "user_id").to_owned();
            let session = string(&lease, "session_id").to_owned();
            if uuid::Uuid::parse_str(&execution).is_err() {
                return Err(ContentError::new(424, "sandbox execution is invalid"));
            }
            let (e, t, u, s) = (
                execution.clone(),
                tenant.clone(),
                user.clone(),
                session.clone(),
            );
            let row:Option<Value>=self.database.run(move|c|Box::pin(async move{sqlx::query_scalar("SELECT to_jsonb(e) FROM assistant_capability_executions e WHERE execution_id=$1::text::uuid AND tenant_id=$2 AND user_id=$3 AND session_id=$4").bind(e).bind(t).bind(u).bind(s).fetch_optional(c).await})).await?;
            let row = row.ok_or_else(|| ContentError::new(403, "capability scope mismatch"))?;
            let hash = format!("sha256:{}", string(&row, "arguments_sha256").trim());
            if lease["arguments_hash"] != hash
                || (!lease["run_id"].is_null() && lease["run_id"] != row["run_id"])
            {
                return Err(ContentError::new(403, "capability scope mismatch"));
            }
            let scope = CapabilityScope {
                tenant_id: tenant,
                user_id: user,
                session_id: session,
                execution_id: execution,
                run_id: string(&row, "run_id").into(),
                tool_call_id: string(&row, "tool_call_id").into(),
                arguments_hash: hash,
            };
            artifact["tool_call_id"] = json!(scope.tool_call_id);
            artifact["arguments_hash"] = json!(scope.arguments_hash);
            persist_capability_artifact(&self.database, &self.artifacts, scope, artifact, false)
                .await
        })
    }
}
pub struct PythonCodeCapability {
    pub transport: Arc<dyn SandboxTransport>,
    pub artifacts: Arc<dyn CodeArtifactSink>,
}
impl PythonCodeCapability {
    pub async fn execute(&self, request: Value) -> Result<Value, ContentError> {
        let err = |s: &str| ContentError::new(424, s);
        let lease = &request["lease"];
        if !lease.is_object() {
            return Err(err("runtime lease is required"));
        }
        for field in ["tenant_id", "user_id", "session_id"] {
            let v = string(lease, field);
            if v.is_empty() || v.chars().count() > 255 || v.chars().any(|c| (c as u32) < 32) {
                return Err(err("runtime lease scope is invalid"));
            }
        }
        if !lease["capability_id"].is_null() && lease["capability_id"] != "execute_python_code" {
            return Err(err("runtime lease capability is invalid"));
        }
        let code = string(&request, "code");
        if code.trim().is_empty() {
            return Err(err("code is required"));
        }
        if code.len() > 2000000 {
            return Err(err("code exceeds host bound"));
        }
        let mut inputs = request
            .get("inputs")
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(json!([]));
        let array = inputs
            .as_array_mut()
            .ok_or_else(|| err("input attachments are malformed"))?;
        let mut input_bytes = 0u64;
        for input in array {
            filename(string(input, "filename"))?;
            let size = crate::rate_config_api::integer(&input["size_bytes"])
                .map_err(|_| err("input attachment is malformed"))?;
            let raw = STANDARD
                .decode(string(input, "content_base64"))
                .map_err(|_| err("input attachment is malformed"))?;
            if size < 0 || size as usize != raw.len() {
                return Err(err("input attachment size mismatch"));
            }
            input["size_bytes"] = json!(size);
            input_bytes += size as u64;
        }
        if input_bytes > 24 * 1024 * 1024 {
            return Err(err("input attachments exceed host bound"));
        }
        let mut limits = json!({"timeout_seconds":30,"memory_bytes":512*1024*1024,"cpu_millis":500,"pids":32,"stdout_bytes":2000000,"stderr_bytes":2000000,"output_bytes":24*1024*1024,"output_files":64});
        if let Some(overrides) = request.get("limits").filter(|v| !v.is_null()) {
            let m = overrides
                .as_object()
                .ok_or_else(|| err("sandbox limits are malformed"))?;
            for (k, v) in m {
                if limits.get(k).is_none() || !v.is_i64() {
                    return Err(err("sandbox limits are malformed"));
                }
                limits[k] = v.clone();
            }
        }
        let n = |k: &str| limits[k].as_i64().unwrap_or(0);
        if !(1..=300).contains(&n("timeout_seconds")) {
            return Err(err("invalid sandbox timeout"));
        }
        if ["memory_bytes", "cpu_millis", "pids"]
            .iter()
            .any(|k| n(k) <= 0)
        {
            return Err(err("invalid sandbox resource limit"));
        }
        if n("stdout_bytes") > 2000000 || n("stderr_bytes") > 2000000 {
            return Err(err("stream limit exceeds host bound"));
        }
        if !(1..=24 * 1024 * 1024).contains(&n("output_bytes"))
            || !(1..=64).contains(&n("output_files"))
        {
            return Err(err("output limit exceeds host bound"));
        }
        let mut arguments = json!({"code":code,"inputs":inputs});
        if request.get("limits").is_some() {
            arguments["limits"] = limits.clone();
        }
        let hash = format!("sha256:{}", sha256(&canonical_json(&arguments)?));
        if request["arguments_hash"] != hash {
            return Err(err("arguments hash mismatch"));
        }
        let mut result=self.transport.execute(json!({"lease":lease,"arguments_hash":hash,"code":code,"inputs":inputs,"limits":limits})).await?;
        let execution = string(&result, "execution_id").to_owned();
        if execution.is_empty() {
            return Err(err("sandbox result missing execution id"));
        }
        if matches!(
            string(&result, "side_effect_state"),
            "side_effect_unknown" | "unknown"
        ) {
            return Err(err("sandbox outcome is unknown"));
        }
        if !matches!(
            string(&result, "status"),
            "succeeded" | "failed" | "timeout" | "cancelled"
        ) {
            return Err(err("sandbox result status is invalid"));
        }
        let files = result
            .get("output_files")
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(json!([]));
        let files = files
            .as_array()
            .ok_or_else(|| err("too many output files"))?;
        if files.len() > 64 {
            return Err(err("too many output files"));
        }
        let (mut total, mut persisted) = (0u64, vec![]);
        for item in files {
            filename(string(item, "filename"))?;
            let size = crate::rate_config_api::integer(&item["size_bytes"])
                .map_err(|_| err("malformed output artifact"))?;
            let content = STANDARD
                .decode(string(item, "content_base64"))
                .map_err(|_| err("malformed output artifact"))?;
            if size < 0 || size as usize != content.len() {
                return Err(err("output artifact exceeds host bound"));
            }
            total += size as u64;
            if total > 24 * 1024 * 1024 {
                return Err(err("output artifacts exceed host bound"));
            }
            if !hex_id(string(item, "sha256"), "", 64)
                || !constant_time_equal(&sha256(&content), string(item, "sha256"))
            {
                return Err(err("output artifact hash mismatch"));
            }
            let mut scope = lease.clone();
            scope["execution_id"] = json!(execution);
            scope["arguments_hash"] = json!(hash);
            persisted.push(self.artifacts.put_code_output(scope, item.clone()).await?);
        }
        result["artifacts"] = json!(persisted);
        result["side_effect_state"] = json!("known");
        Ok(result)
    }
}
pub fn router(broker: Arc<ContentCapabilityBroker>) -> Router {
    Router::new()
        .route(
            "/internal/v2/agent-capabilities/office/artifacts",
            post(office),
        )
        .route(
            "/internal/v2/agent-capabilities/python/artifacts",
            post(python_artifact),
        )
        .route(
            "/internal/v2/agent-capabilities/python/execute",
            post(python_execute),
        )
        .route(
            "/internal/v2/agent-capabilities/image/generate",
            post(image),
        )
        .route(
            "/internal/v2/agent-capabilities/attachments/read",
            post(attachment),
        )
        .with_state(broker)
}
async fn office(
    State(b): State<Arc<ContentCapabilityBroker>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ContentError> {
    let s = b.authorization.authorize(
        &headers,
        "/internal/v2/agent-capabilities/office/artifacts",
        &body,
        chrono::Utc::now().timestamp(),
    )?;
    Ok(Json(b.put_artifact(s, body, true).await?))
}
async fn python_artifact(
    State(b): State<Arc<ContentCapabilityBroker>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ContentError> {
    let s = b.authorization.authorize(
        &headers,
        "/internal/v2/agent-capabilities/python/artifacts",
        &body,
        chrono::Utc::now().timestamp(),
    )?;
    Ok(Json(b.put_artifact(s, body, false).await?))
}
async fn python_execute(
    State(b): State<Arc<ContentCapabilityBroker>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ContentError> {
    b.authorization.peer(&headers)?;
    Ok(Json(b.python.execute(body).await?))
}
async fn image(
    State(b): State<Arc<ContentCapabilityBroker>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ContentError> {
    let s = b.authorization.authorize(
        &headers,
        "/internal/v2/agent-capabilities/image/generate",
        &body,
        chrono::Utc::now().timestamp(),
    )?;
    Ok(Json(b.generate_image(s, body).await?))
}
async fn attachment(
    State(b): State<Arc<ContentCapabilityBroker>>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ContentError> {
    if body.get("operation").is_none() {
        body["operation"] = json!("content")
    }
    if body.get("max_chars").is_none() {
        body["max_chars"] = json!(100000)
    }
    let s = b.authorization.authorize(
        &headers,
        "/internal/v2/agent-capabilities/attachments/read",
        &body,
        chrono::Utc::now().timestamp(),
    )?;
    Ok(Json(
        attachments::read_attachment(
            &b.artifacts,
            &s.tenant_id,
            &s.user_id,
            &s.session_id,
            &body,
            b.pdf.as_ref(),
        )
        .await?,
    ))
}
