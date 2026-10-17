use super::artifacts::{ArtifactStorage, NewArtifact};
use super::storage::ObjectStorage;
use super::{ContentAccess, ContentError, ContentIdentity, hex_id, sha256, string};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read},
    sync::Arc,
};
fn attachment_error(code: &str, status: u16) -> ContentError {
    ContentError::new(status, json!({"code":code}))
}
fn unavailable() -> ContentError {
    attachment_error("ATTACHMENT_UNAVAILABLE", 422)
}
pub fn upload_name<'a>(
    reference: &'a str,
    user: &ContentIdentity,
) -> Result<&'a str, ContentError> {
    let prefix = format!("/uploads/{}/{}/", user.tenant_id, user.user_id);
    let name = reference.strip_prefix(&prefix).ok_or_else(unavailable)?;
    let valid = name.split_once('.').is_some_and(|(stem, ext)| {
        let parts: Vec<_> = stem.split('_').collect();
        parts.len() == 3
            && hex_id(parts[0], "", 8)
            && parts[1].len() == 8
            && parts[2].len() == 6
            && parts[1]
                .bytes()
                .chain(parts[2].bytes())
                .all(|b| b.is_ascii_digit())
            && (1..=8).contains(&ext.len())
            && ext
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    });
    if !valid {
        return Err(unavailable());
    }
    Ok(name)
}
#[derive(Clone)]
pub struct AttachmentBinder {
    pub artifacts: Arc<ArtifactStorage>,
    pub uploads: Arc<dyn ObjectStorage>,
    pub local_fallbacks: Vec<Arc<dyn ObjectStorage>>,
    pub access: Arc<dyn ContentAccess>,
}
impl AttachmentBinder {
    pub async fn bind(
        &self,
        user: &ContentIdentity,
        session: &str,
        model: &str,
        provider: Option<&str>,
        refs: &[String],
    ) -> Result<Vec<String>, ContentError> {
        let mut bound = Vec::new();
        let mut vision = false;
        for reference in refs {
            let (id, is_image) = if hex_id(reference, "art_", 16) {
                let a = self
                    .artifacts
                    .get(reference)
                    .await?
                    .filter(|a| {
                        a.tenant_id == user.tenant_id
                            && a.user_id == user.user_id
                            && a.session_id == session
                            && a.size_bytes > 0
                    })
                    .ok_or_else(unavailable)?;
                (
                    reference.clone(),
                    a.mime_type.as_deref().unwrap_or("").starts_with("image/"),
                )
            } else {
                let name = upload_name(reference, user)?;
                let format = name.rsplit('.').next().unwrap_or("");
                if ![
                    "pdf", "docx", "md", "txt", "csv", "json", "xlsx", "pptx", "png", "jpg",
                    "jpeg", "gif", "webp",
                ]
                .contains(&format)
                {
                    return Err(attachment_error("ATTACHMENT_FORMAT_UNSUPPORTED", 415));
                }
                let image = ["png", "jpg", "jpeg", "gif", "webp"].contains(&format);
                if image && !vision {
                    self.access.require_vision_model(user, model, provider).await?;
                    vision = true;
                }
                let key = reference.trim_start_matches('/');
                let mut content = self.uploads.download_bounded(key, 32 * 1024 * 1024).await;
                let mut used = self.uploads.clone();
                if content.as_ref().err().is_some_and(|e| e.status == 404) {
                    for fallback in &self.local_fallbacks {
                        match fallback.download_bounded(key, 32 * 1024 * 1024).await {
                            Ok(bytes) => {
                                content = Ok(bytes);
                                used = fallback.clone();
                                break;
                            }
                            Err(_) => continue,
                        }
                    }
                }
                let bytes = content.map_err(|e| {
                    if e.status == 413 {
                        attachment_error("ATTACHMENT_TOO_LARGE", 413)
                    } else {
                        unavailable()
                    }
                })?;
                if bytes.is_empty() {
                    return Err(unavailable());
                }
                if bytes.len() > 32 * 1024 * 1024 {
                    return Err(attachment_error("ATTACHMENT_TOO_LARGE", 413));
                }
                let hash = sha256(&bytes);
                let id = format!(
                    "art_{}",
                    &sha256(
                        format!(
                            "{}:{}:{session}:{reference}:{hash}",
                            user.tenant_id, user.user_id
                        )
                        .as_bytes()
                    )[..16]
                );
                let mut metadata = json!({"content_sha256":hash,"upload_ref":reference});
                if let Ok(Some(info)) = used.head(key).await {
                    if let Some(original) = info.metadata.get("original_filename") {
                        if !original.is_empty()
                            && original.chars().count() <= 255
                            && !original.contains(['/', '\\'])
                            && !matches!(original.as_str(), "." | "..")
                            && !original.chars().any(|c| (c as u32) < 32 || c == '\u{7f}')
                        {
                            metadata["original_filename"] = json!(original);
                        }
                    }
                }
                let new: NewArtifact = serde_json::from_value(
                    json!({"session_id":session,"tenant_id":user.tenant_id,"user_id":user.user_id,"type":if image{"image"}else{"file"},"format":format,"title":name,"filename":name,"source":"user","metadata":metadata,"artifact_id":id}),
                )?;
                self.artifacts
                    .create(new, bytes)
                    .await
                    .map_err(|_| attachment_error("ATTACHMENT_BIND_FAILED", 503))?;
                (id, image)
            };
            if is_image && !vision {
                self.access.require_vision_model(user, model, provider).await?;
                vision = true;
            }
            if !bound.contains(&id) {
                bound.push(id)
            }
        }
        Ok(bound)
    }
    pub async fn selected_images(
        &self,
        user: &ContentIdentity,
        session: &str,
        refs: &[String],
    ) -> Result<Vec<String>, ContentError> {
        let mut images = vec![];
        for reference in refs {
            let a = self
                .artifacts
                .get(reference)
                .await?
                .filter(|a| {
                    a.tenant_id == user.tenant_id
                        && a.user_id == user.user_id
                        && a.session_id == session
                        && a.size_bytes > 0
                })
                .ok_or_else(unavailable)?;
            let mime = a.mime_type.as_deref().unwrap_or("").to_lowercase();
            if !["image/png", "image/jpeg", "image/gif", "image/webp"].contains(&mime.as_str()) {
                continue;
            }
            if images.len() >= 5 || a.size_bytes > 2 * 1024 * 1024 {
                return Err(attachment_error("IMAGE_INPUT_TOO_LARGE", 413));
            }
            let bytes = self
                .artifacts
                .backend
                .download_bounded(&a.storage_key, 2 * 1024 * 1024)
                .await
                .map_err(|_| unavailable())?;
            if bytes.len() as i64 != a.size_bytes {
                return Err(unavailable());
            }
            if sniff_image(&bytes) != Some(mime.as_str()) {
                return Err(attachment_error("IMAGE_INPUT_INVALID", 422));
            }
            images.push(format!("data:{mime};base64,{}", STANDARD.encode(bytes)));
        }
        Ok(images)
    }
}
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()) {
        Some("image/webp")
    } else {
        None
    }
}
pub fn image_metadata(raw: &[u8], mime: &str) -> Result<Value, ContentError> {
    let error = || ContentError::new(422, "invalid image");
    let be16 = |b: &[u8]| u16::from_be_bytes([b[0], b[1]]) as u32;
    match mime {
        "image/png" => {
            if raw.len() < 24 || !raw.starts_with(b"\x89PNG\r\n\x1a\n") || &raw[12..16] != b"IHDR" {
                return Err(error());
            }
            Ok(
                json!({"width":u32::from_be_bytes(raw[16..20].try_into().unwrap()),"height":u32::from_be_bytes(raw[20..24].try_into().unwrap()),"format":"png"}),
            )
        }
        "image/gif" => {
            if raw.len() < 10 || (!raw.starts_with(b"GIF87a") && !raw.starts_with(b"GIF89a")) {
                return Err(error());
            }
            Ok(
                json!({"width":u16::from_le_bytes([raw[6],raw[7]]),"height":u16::from_le_bytes([raw[8],raw[9]]),"format":"gif"}),
            )
        }
        "image/jpeg" => {
            if raw.len() < 4 || !raw.starts_with(b"\xff\xd8") {
                return Err(error());
            }
            let mut offset = 2;
            while offset + 9 < raw.len() {
                if raw[offset] != 0xff {
                    offset += 1;
                    continue;
                }
                let marker = raw[offset + 1];
                offset += 2;
                if matches!(marker, 0xd8 | 0xd9) {
                    continue;
                }
                let length = be16(&raw[offset..offset + 2]) as usize;
                if length < 2 || offset + length > raw.len() {
                    return Err(error());
                }
                if (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker) {
                    return Ok(
                        json!({"width":be16(&raw[offset+5..offset+7]),"height":be16(&raw[offset+3..offset+5]),"format":"jpeg"}),
                    );
                }
                offset += length;
            }
            Err(error())
        }
        "image/webp" => {
            if raw.len() < 30
                || !raw.starts_with(b"RIFF")
                || &raw[8..12] != b"WEBP"
                || &raw[12..16] != b"VP8X"
            {
                return Err(error());
            }
            let le24 = |b: &[u8]| 1 + (b[0] as u32 | ((b[1] as u32) << 8) | ((b[2] as u32) << 16));
            Ok(json!({"width":le24(&raw[24..27]),"height":le24(&raw[27..30]),"format":"webp"}))
        }
        _ => Err(error()),
    }
}
pub fn safe_zip_members(raw: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, ContentError> {
    let bad = || ContentError::new(422, "attachment archive is unsafe");
    let mut archive = zip::ZipArchive::new(Cursor::new(raw)).map_err(|_| bad())?;
    if archive.is_empty() || archive.len() > 256 {
        return Err(bad());
    }
    let mut names = BTreeSet::new();
    let mut total = 0;
    for i in 0..archive.len() {
        let info = archive.by_index(i).map_err(|_| bad())?;
        let name = info.name().replace('\\', "/");
        let parts: Vec<_> = name.split('/').collect();
        if name.is_empty()
            || name.starts_with('/')
            || name.contains('\0')
            || parts.iter().any(|s| matches!(*s, "" | "." | ".."))
            || info.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
            || !names.insert(name)
            || info.is_dir()
            || info.size() > 16 * 1024 * 1024
            || (info.compressed_size() == 0 && info.size() > 0)
            || info.size() > info.compressed_size().max(1) * 100
        {
            return Err(bad());
        }
        total += info.size();
        if total > 64 * 1024 * 1024 {
            return Err(bad());
        }
    }
    let mut result = BTreeMap::new();
    for i in 0..archive.len() {
        let mut info = archive.by_index(i).map_err(|_| bad())?;
        let mut bytes = Vec::with_capacity(info.size() as usize);
        info.read_to_end(&mut bytes).map_err(|_| bad())?;
        if bytes.len() as u64 != info.size() {
            return Err(bad());
        }
        result.insert(info.name().replace('\\', "/"), bytes);
    }
    Ok(result)
}
pub use ai_platform_gateway_integrations::migration_content::attachments::xml_reference;
pub fn xml_text(raw: &[u8]) -> Result<String, ContentError> {
    let bad = || ContentError::new(422, "XML cannot be safely parsed");
    if raw.len() > 16 * 1024 * 1024 {
        return Err(bad());
    }
    let text = std::str::from_utf8(raw).map_err(|_| bad())?;
    let upper = text.to_uppercase();
    if upper.contains("<!DOCTYPE") || upper.contains("<!ENTITY") {
        return Err(bad());
    }
    let mut reader = quick_xml::Reader::from_str(text);
    let (mut depth, mut nodes, mut characters, mut roots) = (0usize, 0usize, 0usize, 0usize);
    let mut stack: Vec<(String, bool)> = vec![];
    let mut parts = vec![];
    loop {
        match reader.read_event().map_err(|_| bad())? {
            quick_xml::events::Event::Start(_) => {
                if depth == 0 {
                    roots += 1;
                }
                if let Some(parent) = stack.last_mut() {
                    parent.1 = true;
                }
                depth += 1;
                nodes += 1;
                if roots > 1 || depth > 64 || nodes > 100000 {
                    return Err(bad());
                }
                stack.push((String::new(), false));
            }
            quick_xml::events::Event::Empty(_) => {
                if depth == 0 {
                    roots += 1;
                }
                if let Some(parent) = stack.last_mut() {
                    parent.1 = true;
                }
                nodes += 1;
                if roots > 1 || depth + 1 > 64 || nodes > 100000 {
                    return Err(bad());
                }
            }
            quick_xml::events::Event::Text(e) => {
                let decoded = e.decode().map_err(|_| bad())?;
                let decoded = quick_xml::escape::unescape(&decoded).map_err(|_| bad())?;
                if let Some((current, child)) = stack.last_mut() {
                    if !*child {
                        current.push_str(&decoded)
                    }
                } else if !decoded.trim().is_empty() {
                    return Err(bad());
                }
            }
            quick_xml::events::Event::GeneralRef(e) => {
                let decoded = e.decode().map_err(|_| bad())?;
                let decoded = xml_reference(&decoded)?;
                if let Some((current, child)) = stack.last_mut() {
                    if !*child {
                        current.push_str(&decoded)
                    }
                } else {
                    return Err(bad());
                }
            }
            quick_xml::events::Event::CData(e) => {
                if let Some((current, child)) = stack.last_mut() {
                    if !*child {
                        current.push_str(&e.decode().map_err(|_| bad())?);
                    }
                } else {
                    return Err(bad());
                }
            }
            quick_xml::events::Event::End(_) => {
                let (text, _) = stack.pop().ok_or_else(bad)?;
                let text = text.trim();
                if !text.is_empty() {
                    characters += text.chars().count();
                    if characters > 500000 {
                        return Err(bad());
                    }
                    parts.push(text.to_owned());
                }
                depth = depth.checked_sub(1).ok_or_else(bad)?;
            }
            quick_xml::events::Event::DocType(_) => return Err(bad()),
            quick_xml::events::Event::Eof => break,
            _ => (),
        }
    }
    if depth != 0 || roots != 1 {
        return Err(bad());
    }
    Ok(parts.join(" "))
}
// One shared parser/error contract; backend and attachment authority remain separate.
pub use ai_platform_gateway_integrations::migration_content::attachments::PdfAttachmentParser;
/// Explicit source missing-pypdf branch. This makes broker construction possible
/// without implying that PDF extraction is available or selecting another parser.
#[derive(Clone, Copy, Default)]
pub struct UnavailablePdfAttachmentParser;
impl UnavailablePdfAttachmentParser {
    pub const fn available(&self) -> bool {
        false
    }
}
impl PdfAttachmentParser for UnavailablePdfAttachmentParser {
    fn extract_strict(
        &self,
        _raw: &[u8],
        _max_pages: usize,
    ) -> Result<(String, usize), ContentError> {
        Err(ContentError::new(
            422,
            "PDF cannot be safely parsed: strict pypdf backend is unavailable",
        ))
    }
}

pub fn extract_content(
    raw: &[u8],
    filename: &str,
    mime: &str,
    max: usize,
    pdf: &dyn PdfAttachmentParser,
) -> Result<(Value, Option<String>, bool), ContentError> {
    let suffix = filename
        .rsplit_once('.')
        .map(|(_, s)| s.to_lowercase())
        .unwrap_or_default();
    let (meta, text) = if mime == "text/plain"
        || ["txt", "md", "csv", "json"].contains(&suffix.as_str())
    {
        let text = std::str::from_utf8(raw)
            .map_err(|_| ContentError::new(422, "attachment encoding invalid"))?
            .to_owned();
        (
            json!({"format":if suffix.is_empty(){"txt"}else{&suffix},"encoding":"utf-8","characters":text.chars().count()}),
            text,
        )
    } else if ["image/png", "image/jpeg", "image/gif", "image/webp"].contains(&mime) {
        return Ok((image_metadata(raw, mime)?, None, false));
    } else if mime == "application/pdf" || suffix == "pdf" {
        let (text, pages) = pdf
            .extract_strict(raw, 100)
            .map_err(|_| ContentError::new(422, "PDF cannot be safely parsed"))?;
        if pages > 100 {
            return Err(ContentError::new(422, "PDF page limit"));
        }
        (json!({"format":"pdf","pages":pages}), text)
    } else {
        let format = match mime {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
            "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
            _ => suffix.as_str(),
        };
        if !["docx", "xlsx", "pptx"].contains(&format) {
            return Err(ContentError::new(422, "unsupported attachment format"));
        }
        let members = safe_zip_members(raw)?;
        let names: Vec<_> = members
            .keys()
            .filter(|s| match format {
                "docx" => s.as_str() == "word/document.xml",
                "xlsx" => s.starts_with("xl/worksheets/") && s.ends_with(".xml"),
                _ => s.starts_with("ppt/slides/") && s.ends_with(".xml"),
            })
            .collect();
        if names.is_empty() {
            return Err(ContentError::new(422, "OOXML main part missing"));
        }
        let text = names
            .iter()
            .map(|name| xml_text(&members[*name]))
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");
        (json!({"format":format,"parts":names.len()}), text)
    };
    let truncated = text.chars().count() > max;
    Ok((meta, Some(text.chars().take(max).collect()), truncated))
}
pub async fn read_attachment(
    storage: &ArtifactStorage,
    tenant: &str,
    user: &str,
    session: &str,
    body: &Value,
    pdf: &dyn PdfAttachmentParser,
) -> Result<Value, ContentError> {
    let id = string(body, "attachment_id");
    if !id.strip_prefix("art_").is_some_and(|s| {
        (8..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
    }) {
        return Err(ContentError::new(404, "attachment not found"));
    }
    let max = body
        .get("max_chars")
        .and_then(Value::as_u64)
        .unwrap_or(100000) as usize;
    if !(1..=500000).contains(&max)
        || !matches!(
            body.get("operation")
                .and_then(Value::as_str)
                .unwrap_or("content"),
            "metadata" | "content"
        )
    {
        return Err(ContentError::new(422, "invalid attachment request"));
    }
    let result=async{let a=storage.get(id).await?.filter(|a|a.tenant_id==tenant&&a.user_id==user&&a.session_id==session&&a.size_bytes>0&&a.size_bytes<=32*1024*1024).ok_or_else(unavailable)?;if a.filename.is_empty()||a.filename.contains(['/','\\'])||a.filename.chars().any(|c|(c as u32)<32){return Err(unavailable())}let raw=storage.backend.download_bounded(&a.storage_key,32*1024*1024).await?;if raw.len() as i64!=a.size_bytes||raw.len()>32*1024*1024{return Err(unavailable())}let mime=a.mime_type.as_deref().unwrap_or("application/octet-stream").to_lowercase();let(meta,content,truncated)=extract_content(&raw,&a.filename,&mime,max,pdf)?;let mut metadata=if a.metadata.is_object(){a.metadata}else{json!({})};for(k,v)in meta.as_object().unwrap(){metadata[k]=v.clone();}let wants=body.get("operation").and_then(Value::as_str).unwrap_or("content")=="content";let result=json!({"attachment_id":id,"filename":a.filename,"mime_type":mime,"format":meta["format"],"size_bytes":raw.len(),"sha256":sha256(&raw),"metadata":metadata,"content":if wants{content}else{None},"truncated":wants&&truncated});if serde_json::to_vec(&result)?.len()>8*1024*1024{return Err(unavailable())}Ok(result)}.await;
    result.map_err(|_: ContentError| ContentError::new(422, "attachment cannot be safely parsed"))
}

impl crate::migration_agent::routes::AgentAttachments for AttachmentBinder {
    fn bind<'a>(
        &'a self,
        actor: &'a crate::migration_agent::Actor,
        scope: &'a crate::migration_agent::Scope,
        model: &'a str,
        provider: Option<&'a str>,
        refs: &'a [String],
    ) -> crate::migration_agent::AgentFuture<'a, (Vec<String>, Vec<String>)> {
        Box::pin(async move {
            let user = ContentIdentity {
                tenant_id: actor.tenant_id.clone(),
                user_id: actor.user_id.clone(),
                roles: actor.roles.clone(),
                tier: actor.tier.clone(),
                is_authenticated: true,
                ..Default::default()
            };
            if scope.tenant_id != user.tenant_id || scope.user_id != user.user_id {
                return Err(crate::migration_agent::AgentError::code(
                    403,
                    "ATTACHMENT_UNAVAILABLE",
                ));
            }
            let bound = AttachmentBinder::bind(self, &user, &scope.session_id, model, provider, refs)
                .await
                .map_err(|e| crate::migration_agent::AgentError {
                    status: e.status,
                    detail: e.detail,
                })?;
            let images = self
                .selected_images(&user, &scope.session_id, &bound)
                .await
                .map_err(|e| crate::migration_agent::AgentError {
                    status: e.status,
                    detail: e.detail,
                })?;
            Ok((bound, images))
        })
    }
}

#[cfg(test)]
mod shared_pdf_port_identity_tests {
    use super::*;
    #[test]
    fn legacy_gateway_path_and_domain_contract_are_the_same_trait() {
        let gateway:Box<dyn PdfAttachmentParser>=Box::new(UnavailablePdfAttachmentParser);
        let shared:Box<dyn ai_platform_gateway_integrations::migration_content::attachments::PdfAttachmentParser>=gateway;
        let error:ai_platform_gateway_integrations::migration_content::ContentError=shared.extract_strict(b"not-pdf",1).unwrap_err();assert_eq!(error.status,422);assert_eq!(error.detail,"PDF cannot be safely parsed: strict pypdf backend is unavailable");
    }
}
