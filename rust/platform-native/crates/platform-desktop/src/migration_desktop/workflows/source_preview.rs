//! Historical citations are shown only after current-access and SHA-256 identity checks.
use super::super::api::{ApiError, ApiRequest, DesktopApi};
use super::common::*;
use eframe::egui;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
#[derive(Clone)]
pub struct CitationIdentity {
    pub dataset: String,
    pub document: String,
    pub excerpt: String,
    pub provenance: &'static str,
    pub version: Option<u64>,
    pub hash: Option<String>,
}
fn present<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.get(key).filter(|v| !v.is_null())
}
pub fn citation_identity(dataset: &str, hit: &Value) -> CitationIdentity {
    let meta = &hit["metadata"];
    let top_doc = present(hit, "document_id");
    let meta_doc = present(meta, "document_id");
    let mut source = CitationIdentity {
        dataset: dataset.into(),
        document: top_doc
            .or(meta_doc)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .into(),
        excerpt: hit["content"]
            .as_str()
            .or_else(|| hit["text"].as_str())
            .unwrap_or("")
            .into(),
        provenance: "invalid",
        version: None,
        hash: None,
    };
    if source.dataset.is_empty()
        || source.document.is_empty()
        || present(hit, "dataset_id").is_some_and(|v| v != dataset)
        || present(meta, "dataset_id").is_some_and(|v| v != dataset)
        || (top_doc.is_some() && meta_doc.is_some() && top_doc != meta_doc)
    {
        return source;
    }
    let tv = present(hit, "source_version");
    let th = present(hit, "source_hash");
    let mv = present(meta, "source_version");
    let mh = present(meta, "source_hash");
    if tv.is_some() != th.is_some() || mv.is_some() != mh.is_some() {
        return source;
    }
    if tv.is_none() && mv.is_none() {
        source.provenance = "legacy";
        return source;
    }
    let version = tv.or(mv).and_then(Value::as_u64);
    let hash = th.or(mh).and_then(Value::as_str);
    if tv.is_some() && mv.is_some() && tv != mv
        || th.is_some()
            && mh.is_some()
            && th.and_then(Value::as_str).map(str::to_lowercase)
                != mh.and_then(Value::as_str).map(str::to_lowercase)
        || version.is_none_or(|v| v == 0)
        || hash.is_none_or(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return source;
    }
    source.provenance = "versioned";
    source.version = version;
    source.hash = hash.map(str::to_ascii_lowercase);
    source
}
#[derive(Default)]
pub struct SourcePreview {
    pub job: RemoteJob,
    source: Option<CitationIdentity>,
    content: String,
    title: String,
    state: String,
    next_check: Option<Instant>,
}
impl SourcePreview {
    pub fn open(&mut self, source: CitationIdentity) {
        *self = Self::default();
        self.state = if source.provenance == "invalid" {
            "unavailable"
        } else {
            "loading"
        }
        .into();
        self.next_check = if source.provenance == "invalid" {
            None
        } else {
            Some(Instant::now())
        };
        self.source = Some(source);
    }
    pub fn close(&mut self) {
        *self = Self::default();
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        if let Some(message) = self.job.poll() {
            match message.result {
                Ok(value) => {
                    self.content = value["content"].as_str().unwrap_or("").into();
                    self.title = value["title"].as_str().unwrap_or("").into();
                    self.state = value["state"].as_str().unwrap_or("unavailable").into();
                }
                Err(_) => {
                    self.content.clear();
                    self.title.clear();
                    self.state = "unavailable".into();
                }
            }
            self.next_check = Some(Instant::now() + Duration::from_secs(3));
        }
        let Some(source) = self.source.clone() else {
            return;
        };
        if source.provenance != "invalid"
            && !self.job.busy
            && self.next_check.is_some_and(|at| at <= Instant::now())
        {
            self.next_check = None;
            let api = api.clone();
            let source = source.clone();
            self.job.start(
                "load_source",
                async move {
                    if source.provenance == "versioned" {
                        let version = source.version.ok_or(ApiError::Request)?;
                        let hash = source.hash.ok_or(ApiError::Request)?;
                        let value = api
                            .execute(
                                ApiRequest::get(&[
                                    "api",
                                    "v1",
                                    "knowledge",
                                    &source.dataset,
                                    "documents",
                                    &source.document,
                                    "versions",
                                    &version.to_string(),
                                    "source",
                                ])
                                .query("hash", &hash),
                            )
                            .await?;
                        let content = value["content"].as_str().ok_or(ApiError::Response)?;
                        let actual = format!("{:x}", Sha256::digest(content.as_bytes()));
                        if value["document_id"] != source.document
                            || value["version_number"].as_u64() != Some(version)
                            || actual != hash
                        {
                            return Err(ApiError::Response);
                        }
                        Ok(json!({"title":value["title"],"content":content,"state":"verified"}))
                    } else {
                        let value = api
                            .execute(ApiRequest::get(&[
                                "api",
                                "v1",
                                "knowledge",
                                &source.dataset,
                                "documents",
                                &source.document,
                                "source",
                            ]))
                            .await?;
                        Ok(json!({"title":value["title"],"state":"excerpt"}))
                    }
                },
                runtime,
                ctx,
            );
        }
        if let Some(at) = self.next_check {
            ctx.request_repaint_after(at.saturating_duration_since(Instant::now()));
        }
        egui::Frame::group(ui.style()).show(ui,|ui|{ui.strong(if self.title.is_empty(){"Citation source"}else{&self.title});ui.label(format!("Dataset {} · document {}",source.dataset,source.document));match self.state.as_str(){"verified"=>{ui.label(format!("Saved version #{} verified against its hash and your current access",source.version.unwrap_or(0)));ui.label(&source.excerpt);egui::ScrollArea::vertical().id_salt("verified-source").max_height(400.0).show(ui,|ui|{ui.label(&self.content);});},"excerpt"=>{ui.label(&source.excerpt);ui.label("Only the saved excerpt is available; the historical full document cannot be verified.");},"loading"=>{ui.spinner();},_=>{ui.label("This source is no longer available to you or its saved identity could not be verified.");}}if ui.button("Close source preview").clicked(){self.close();}});
    }
}
