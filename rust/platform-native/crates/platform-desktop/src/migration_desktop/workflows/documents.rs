//! Dataset settings, metadata registries, document/segment operations and version history.
use super::super::api::{ApiRequest, DesktopApi};
use super::common::*;
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
const CHUNK_FIELDS: &[&str] = &[
    "mode",
    "chunk_size",
    "chunk_overlap",
    "overlap",
    "max_chunk_size",
    "min_chunk_size",
    "use_token_count",
    "token_limit",
    "max_tokens",
    "min_chunk_tokens",
    "max_chunk_tokens",
    "parent_token_limit",
    "child_token_limit",
    "separators",
    "separator",
    "primary_separator",
    "keep_separator",
    "heading_level",
    "heading_patterns",
    "min_paragraph_length",
    "merge_short_paragraphs",
    "parent_mode",
    "parent_chunk_size",
    "parent_overlap",
    "parent_chunk_overlap",
    "child_chunk_size",
    "child_overlap",
    "child_chunk_overlap",
    "question_prefix",
    "answer_prefix",
    "remove_extra_spaces",
    "remove_urls_emails",
    "normalize_whitespace",
    "strip_html",
    "extract_metadata",
    "metadata_fields",
    "page_marker",
    "strict_section_traceability",
    "preserve_images",
    "image_context_chars",
    "segmentation",
];
#[derive(Default)]
struct MetadataField {
    name: String,
    label: String,
    kind: String,
    description: String,
    value: String,
    touched: bool,
    remove: bool,
}
pub struct DocumentEditor {
    pub job: RemoteJob,
    review: ReviewedAction,
    bound: String,
    dataset: Value,
    result: Value,
    documents: Value,
    document_id: String,
    loaded_doc: String,
    document: Value,
    offset: u64,
    limit: u64,
    enabled: bool,
    archived: bool,
    archive_reason: String,
    registry_revision: Option<u64>,
    fields: Vec<MetadataField>,
    batch_ids: String,
    all_documents: bool,
    batch_id: String,
    batch: Value,
    batch_unknown: bool,
    next_batch_poll: Option<Instant>,
    versions: Value,
    version_from: u64,
    version_to: u64,
    restore_version: u64,
    version_offset: u64,
    segments: Value,
    segment_id: String,
    segment_text: String,
    segment_answer: String,
    segment_keywords: String,
    segment_enabled: bool,
    segment_search: String,
    segment_offset: u64,
    segment_ids: String,
    config: Value,
    chunk_mode: String,
    chunk_size: u64,
    chunk_overlap: u64,
    chunk_advanced: String,
    retrieval_patch: String,
    preview_text: String,
    dataset_name: String,
    dataset_description: String,
    visibility: String,
    impact: Value,
    password: String,
    delete_reason: String,
}
impl Default for DocumentEditor {
    fn default() -> Self {
        Self {
            job: RemoteJob::default(),
            review: ReviewedAction::default(),
            bound: String::new(),
            dataset: Value::Null,
            result: Value::Null,
            documents: Value::Null,
            document_id: String::new(),
            loaded_doc: String::new(),
            document: Value::Null,
            offset: 0,
            limit: 50,
            enabled: true,
            archived: false,
            archive_reason: String::new(),
            registry_revision: None,
            fields: Vec::new(),
            batch_ids: String::new(),
            all_documents: false,
            batch_id: String::new(),
            batch: Value::Null,
            batch_unknown: false,
            next_batch_poll: None,
            versions: Value::Null,
            version_from: 1,
            version_to: 2,
            restore_version: 1,
            version_offset: 0,
            segments: Value::Null,
            segment_id: String::new(),
            segment_text: String::new(),
            segment_answer: String::new(),
            segment_keywords: String::new(),
            segment_enabled: true,
            segment_search: String::new(),
            segment_offset: 0,
            segment_ids: String::new(),
            config: Value::Null,
            chunk_mode: "automatic".into(),
            chunk_size: 500,
            chunk_overlap: 50,
            chunk_advanced: "{}".into(),
            retrieval_patch: "{}".into(),
            preview_text: String::new(),
            dataset_name: String::new(),
            dataset_description: String::new(),
            visibility: "private".into(),
            impact: Value::Null,
            password: String::new(),
            delete_reason: String::new(),
        }
    }
}
fn array<'a>(value: &'a Value, key: &str) -> Option<&'a Vec<Value>> {
    value.as_array().or_else(|| value[key].as_array())
}

fn valid_impact(value: &Value, dataset: &str) -> bool {
    if value["dataset_id"].as_str() != Some(dataset) {
        return false;
    }
    let Some(hidden) = value["hidden_agent_count"].as_u64() else {
        return false;
    };
    let Some(agents) = value["visible_agents"].as_array() else {
        return false;
    };
    let mut ids = std::collections::HashSet::new();
    if !agents.iter().all(|a| {
        a["agent_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty() && ids.insert(id))
            && a["name"].is_string()
            && a["current_draft"].is_boolean()
            && a["active_publication"].is_boolean()
            && a["historical_version_count"].as_u64().is_some()
    }) {
        return false;
    }
    for key in ["current_draft", "active_publication", "historical_version"] {
        let count = &value["counts"][key];
        let Some(visible) = count["visible"].as_u64() else {
            return false;
        };
        let Some(hidden_count) = count["hidden"].as_u64() else {
            return false;
        };
        let actual = agents
            .iter()
            .filter(|a| {
                if key == "historical_version" {
                    a["historical_version_count"].as_u64().unwrap_or(0) > 0
                } else {
                    a[key] == true
                }
            })
            .count() as u64;
        if visible != actual || hidden_count > hidden {
            return false;
        }
    }
    true
}
impl DocumentEditor {
    fn accept_doc(&mut self, v: &Value) {
        self.loaded_doc = v["document_id"]
            .as_str()
            .or_else(|| v["id"].as_str())
            .unwrap_or("")
            .into();
        self.document_id = self.loaded_doc.clone();
        self.document = v.clone();
        self.enabled = v["enabled"].as_bool().unwrap_or(true);
        self.archived = v["archived"].as_bool().unwrap_or(false);
        self.archive_reason = v["archived_reason"].as_str().unwrap_or("").into();
        self.seed_metadata();
        self.review.clear();
    }
    fn seed_metadata(&mut self) {
        for field in &mut self.fields {
            field.value = self.document["metadata"][&field.name]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    let value = &self.document["metadata"][&field.name];
                    if value.is_null() {
                        String::new()
                    } else {
                        value.to_string()
                    }
                });
            field.touched = !self.loaded_doc.is_empty();
            field.remove = false;
        }
    }
    fn accept_registry(&mut self, v: &Value) {
        self.registry_revision = v["revision"].as_u64();
        self.fields = v["fields"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| MetadataField {
                        name: row["name"].as_str().unwrap_or("").into(),
                        label: row["label"].as_str().unwrap_or("").into(),
                        kind: row["type"].as_str().unwrap_or("string").into(),
                        description: row["description"].as_str().unwrap_or("").into(),
                        ..Default::default()
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.seed_metadata();
    }
    fn poll(&mut self) {
        if let Some(message) = self.job.poll() {
            if let Ok(value) = message.result {
                match message.kind.as_str() {
                    "load_dataset" | "save_dataset" => {
                        self.dataset_name = value["name"].as_str().unwrap_or("").into();
                        self.dataset_description =
                            value["description"].as_str().unwrap_or("").into();
                        self.visibility = value["visibility"].as_str().unwrap_or("private").into();
                        self.dataset = value.clone();
                    }
                    "load_documents" => self.documents = value.clone(),
                    "load_document" | "save_metadata" | "document_status" | "archive" => {
                        self.accept_doc(&value)
                    }
                    "load_registry" | "save_registry" => self.accept_registry(&value),
                    "load_versions" => {
                        self.versions = value.clone();
                        self.version_to = value["current_version"].as_u64().unwrap_or(1);
                    }
                    "load_segments" => self.segments = value.clone(),
                    "load_config" => {
                        self.config = value.clone();
                        self.chunk_mode = value["chunking"]["mode"]
                            .as_str()
                            .unwrap_or("automatic")
                            .into();
                        self.chunk_size = value["chunking"]["chunk_size"].as_u64().unwrap_or(500);
                        self.chunk_overlap =
                            value["chunking"]["chunk_overlap"].as_u64().unwrap_or(50);
                        self.chunk_advanced = serde_json::to_string_pretty(&value["chunking"])
                            .unwrap_or_else(|_| "{}".into());
                        self.retrieval_patch = "{}".into();
                    }
                    "load_impact" => self.impact = value.clone(),
                    "batch_reindex" | "batch_delete" | "load_batch" => {
                        self.batch_id = value["operation_id"].as_str().unwrap_or("").into();
                        self.batch = value.clone();
                        let terminal = ["completed", "partial", "failed"]
                            .contains(&value["status"].as_str().unwrap_or(""));
                        self.batch_unknown = !terminal;
                        self.next_batch_poll = if terminal || self.batch_id.is_empty() {
                            None
                        } else {
                            Some(Instant::now() + Duration::from_millis(500))
                        };
                    }
                    _ => {}
                }
                self.result = value;
            } else if ["load_batch", "batch_reindex", "batch_delete"]
                .contains(&message.kind.as_str())
            {
                self.next_batch_poll = None;
                self.batch_unknown = true;
            }
        }
    }
    fn metadata_body(&self) -> Result<Value, String> {
        let revision = self
            .registry_revision
            .ok_or("Load the metadata registry first")?;
        let mut patch = json!({});
        let mut remove = Vec::new();
        for field in &self.fields {
            if !field.touched {
                continue;
            }
            if field.remove {
                remove.push(field.name.clone());
                continue;
            }
            let value = if field.kind == "number" {
                let n = field
                    .value
                    .parse::<f64>()
                    .map_err(|_| format!("{} must be a finite number", field.label))?;
                if !n.is_finite() {
                    return Err(format!("{} must be finite", field.label));
                }
                json!(n)
            } else {
                json!(field.value)
            };
            patch[&field.name] = value;
        }
        Ok(
            json!({"metadata_patch":patch,"metadata_remove":remove,"metadata_schema_revision":revision}),
        )
    }
    fn chunk_body(&self) -> Result<Value, String> {
        if self.chunk_mode == "regex" {
            return Err("Regex chunking is disabled".into());
        }
        if self.chunk_overlap >= self.chunk_size {
            return Err("Overlap must be smaller than chunk size".into());
        }
        let advanced =
            json_object(&self.chunk_advanced).map_err(|_| "Chunking config must be an object")?;
        let mut body = json!({});
        for key in CHUNK_FIELDS {
            if let Some(value) = advanced.get(*key) {
                if *key == "heading_patterns" {
                    let safe = json!([
                        r"^#{1,6}\s+.+$",
                        r"^第[一二三四五六七八九十\d]+[章节条款]",
                        r"^[A-Z][A-Z \t]{4,}:?$"
                    ]);
                    if value != &safe {
                        continue;
                    }
                }
                body[*key] = value.clone();
            }
        }
        body["mode"] = json!(self.chunk_mode);
        body["chunk_size"] = json!(self.chunk_size);
        body["chunk_overlap"] = json!(self.chunk_overlap);
        Ok(body)
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        dataset: &str,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        self.poll();
        if self.bound != dataset {
            if self.job.busy {
                ui.label("An operation on the previous dataset is pending.");
                return;
            }
            let expired = self.job.expired;
            *self = Self::default();
            self.bound = dataset.into();
            self.job.expired = expired;
        }
        if !self.job.busy && self.next_batch_poll.is_some_and(|t| Instant::now() >= t) {
            self.next_batch_poll = None;
            self.job.request(
                "load_batch",
                api,
                ApiRequest::get(&[
                    "api",
                    "v1",
                    "knowledge",
                    &self.bound,
                    "document-batches",
                    &self.batch_id,
                ]),
                runtime,
                ctx,
            );
        }
        if let Some(at) = self.next_batch_poll {
            ctx.request_repaint_after(at.saturating_duration_since(Instant::now()));
        }
        egui::CollapsingHeader::new("Documents, versions and dataset settings").show(ui, |ui| {
            self.job.status_ui(ui);
            ui.label(format!("Dataset: {}", self.bound));
            ui.add_enabled_ui(!self.bound.is_empty() && !self.job.busy, |ui| {
                self.dataset_ui(ui, api, runtime, ctx);
                egui::CollapsingHeader::new("Documents and lifecycle")
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.add(egui::DragValue::new(&mut self.offset).prefix("Offset "));
                        ui.add(
                            egui::DragValue::new(&mut self.limit)
                                .range(1..=200)
                                .prefix("Limit "),
                        );
                        if ui.button("Load document page").clicked() {
                            self.job.request(
                                "load_documents",
                                api,
                                ApiRequest::get(&[
                                    "api",
                                    "v1",
                                    "knowledge",
                                    &self.bound,
                                    "documents",
                                ])
                                .query("limit", self.limit)
                                .query("offset", self.offset),
                                runtime,
                                ctx,
                            );
                        }
                        if let Some(rows) = array(&self.documents, "items") {
                            let mut pick = None;
                            for row in rows {
                                let id = row["document_id"]
                                    .as_str()
                                    .or_else(|| row["id"].as_str())
                                    .unwrap_or("");
                                if ui
                                    .selectable_label(
                                        self.document_id == id,
                                        row["title"].as_str().unwrap_or(id),
                                    )
                                    .clicked()
                                {
                                    pick = Some(row.clone());
                                }
                            }
                            if let Some(v) = pick {
                                self.accept_doc(&v);
                                self.versions = Value::Null;
                            }
                        }
                        text_field(ui, "Document ID", &mut self.document_id);
                        if ui
                            .add_enabled(
                                !self.document_id.is_empty(),
                                egui::Button::new("Load full document"),
                            )
                            .clicked()
                        {
                            self.review.clear();
                            self.job.request(
                                "load_document",
                                api,
                                ApiRequest::get(&[
                                    "api",
                                    "v1",
                                    "knowledge",
                                    &self.bound,
                                    "documents",
                                    &self.document_id,
                                ]),
                                runtime,
                                ctx,
                            );
                        }
                        ui.add_enabled_ui(
                            !self.loaded_doc.is_empty() && self.loaded_doc == self.document_id,
                            |ui| {
                                ui.label(format!(
                                    "Status: {}",
                                    self.document["display_status"]
                                        .as_str()
                                        .or_else(|| self.document["status"].as_str())
                                        .unwrap_or("unknown")
                                ));
                                ui.checkbox(&mut self.enabled, "Enabled for retrieval");
                                ui.checkbox(&mut self.archived, "Archived");
                                text_field(
                                    ui,
                                    "Archive reason (max 255 characters)",
                                    &mut self.archive_reason,
                                );
                                ui.horizontal_wrapped(|ui| {
                                    if ui.button("Apply document status…").clicked() {
                                        self.review.prepare(
                                            "document_status",
                                            "Change document retrieval availability",
                                            ApiRequest::mutation(
                                                Method::PATCH,
                                                &[
                                                    "api",
                                                    "v1",
                                                    "knowledge",
                                                    &self.bound,
                                                    "documents",
                                                    &self.loaded_doc,
                                                    "status",
                                                ],
                                                Some(json!({"enabled":self.enabled})),
                                            ),
                                        );
                                    }
                                    if ui
                                        .add_enabled(
                                            self.archive_reason.chars().count() <= 255,
                                            egui::Button::new("Apply archive state…"),
                                        )
                                        .clicked()
                                    {
                                        let mut body = json!({"archived":self.archived});
                                        if self.archived {
                                            body["reason"] = optional_text(&self.archive_reason);
                                        }
                                        self.review.prepare(
                                            "archive",
                                            "Change document archive state",
                                            ApiRequest::mutation(
                                                Method::PATCH,
                                                &[
                                                    "api",
                                                    "v1",
                                                    "knowledge",
                                                    &self.bound,
                                                    "documents",
                                                    &self.loaded_doc,
                                                    "archive",
                                                ],
                                                Some(body),
                                            ),
                                        );
                                    }
                                    if ui.button("Delete document…").clicked() {
                                        self.review.prepare(
                                            "delete_document",
                                            "Delete this document and its segments",
                                            ApiRequest::mutation(
                                                Method::DELETE,
                                                &[
                                                    "api",
                                                    "v1",
                                                    "knowledge",
                                                    &self.bound,
                                                    "documents",
                                                    &self.loaded_doc,
                                                ],
                                                None,
                                            ),
                                        );
                                    }
                                });
                                self.versions_ui(ui, api, runtime, ctx);
                            },
                        );
                    });
                self.metadata_ui(ui, api, runtime, ctx);
                self.segments_ui(ui, api, runtime, ctx);
                self.config_ui(ui, api, runtime, ctx);
                self.batch_ui(ui, api, runtime, ctx);
            });
            self.review.ui(ui, &mut self.job, api, runtime, ctx);
            if !self.result.is_null() {
                json_view(ui, &self.result);
            }
        });
    }
    fn dataset_ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        egui::CollapsingHeader::new("Dataset identity, visibility and deletion").show(ui,|ui|{if ui.button("Load dataset settings").clicked(){self.job.request("load_dataset",api,ApiRequest::get(&["api","v1","knowledge","datasets",&self.bound]),runtime,ctx);}if self.dataset.is_object(){text_field(ui,"Dataset name",&mut self.dataset_name);text_field(ui,"Description",&mut self.dataset_description);select_text(ui,"dataset_visibility",&mut self.visibility,&["private","tenant","public"]);ui.label(format!("Current permission: {}",self.dataset["my_permission"].as_str().unwrap_or("unknown")));if ui.button("Save dataset settings…").clicked(){self.review.prepare("save_dataset","Update dataset name, description and visibility",ApiRequest::mutation(Method::PUT,&["api","v1","knowledge","datasets",&self.bound],Some(json!({"name":self.dataset_name,"description":self.dataset_description,"visibility":self.visibility}))));}if ui.button("Inspect dependent agents before deletion").clicked(){self.job.request("load_impact",api,ApiRequest::get(&["api","v1","knowledge","datasets",&self.bound,"impact"]),runtime,ctx);}if !self.impact.is_null(){json_view(ui,&self.impact);}ui.label("Deletion requires dataset ownership, current impact review and your password.");ui.add(egui::TextEdit::singleline(&mut self.password).password(true));text_field(ui,"Deletion reason",&mut self.delete_reason);if ui.add_enabled(self.dataset["my_permission"]=="owner"&&valid_impact(&self.impact,&self.bound)&&!self.password.is_empty(),egui::Button::new("Review permanent dataset deletion")).clicked(){self.review.prepare_sensitive("delete_dataset","Permanently delete this dataset; dependent agents may stop working",ApiRequest::mutation(Method::DELETE,&["api","v1","knowledge","datasets",&self.bound],Some(json!({"password":std::mem::take(&mut self.password),"reason":self.delete_reason}))),true);}}});
    }
    fn versions_ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        egui::CollapsingHeader::new("Version history and restore").show(ui, |ui| {
            ui.add(egui::DragValue::new(&mut self.version_offset).prefix("Version offset "));
            if ui.button("Load versions").clicked() {
                self.job.request(
                    "load_versions",
                    api,
                    ApiRequest::get(&[
                        "api",
                        "v1",
                        "knowledge",
                        &self.bound,
                        "documents",
                        &self.loaded_doc,
                        "versions",
                    ])
                    .query("limit", 20)
                    .query("offset", self.version_offset),
                    runtime,
                    ctx,
                );
            }
            if let Some(rows) = self.versions["versions"].as_array() {
                for row in rows {
                    let n = row["version_number"].as_u64().unwrap_or(0);
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "Version {n} · {} · {}",
                            row["change_type"], row["content_hash"]
                        ));
                        if ui.button("Compare from").clicked() {
                            self.version_from = n;
                        }
                        if ui.button("Compare to").clicked() {
                            self.version_to = n;
                        }
                        if ui.button("Restore this…").clicked() {
                            self.restore_version = n;
                        }
                    });
                }
            }
            ui.add(
                egui::DragValue::new(&mut self.version_from)
                    .range(1..=u64::MAX)
                    .prefix("From "),
            );
            ui.add(
                egui::DragValue::new(&mut self.version_to)
                    .range(1..=u64::MAX)
                    .prefix("To "),
            );
            if ui
                .add_enabled(
                    self.version_from != self.version_to,
                    egui::Button::new("Compare versions"),
                )
                .clicked()
            {
                self.job.request(
                    "load_diff",
                    api,
                    ApiRequest::get(&[
                        "api",
                        "v1",
                        "knowledge",
                        &self.bound,
                        "documents",
                        &self.loaded_doc,
                        "versions",
                        "compare",
                    ])
                    .query("from_version", self.version_from)
                    .query("to_version", self.version_to),
                    runtime,
                    ctx,
                );
            }
            ui.add(
                egui::DragValue::new(&mut self.restore_version)
                    .range(1..=u64::MAX)
                    .prefix("Restore version "),
            );
            if ui.button("Restore as a new document version…").clicked() {
                self.review.prepare(
                    "restore_version",
                    "Restore this historical document version as a new version",
                    ApiRequest::mutation(
                        Method::POST,
                        &[
                            "api",
                            "v1",
                            "knowledge",
                            &self.bound,
                            "documents",
                            &self.loaded_doc,
                            "versions",
                            &self.restore_version.to_string(),
                            "restore",
                        ],
                        None,
                    ),
                );
            }
        });
    }
    fn metadata_ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        egui::CollapsingHeader::new("Metadata schema and document values").show(ui,|ui|{if ui.button("Load metadata registry").clicked(){self.job.request("load_registry",api,ApiRequest::get(&["api","v1","knowledge",&self.bound,"metadata-schema"]),runtime,ctx);}if let Some(revision)=self.registry_revision{ui.label(format!("Schema revision {revision}"));let mut remove=None;for(index,field)in self.fields.iter_mut().enumerate(){ui.push_id(index,|ui|{egui::Frame::group(ui.style()).show(ui,|ui|{text_field(ui,"Field name",&mut field.name);text_field(ui,"Label",&mut field.label);select_text(ui,"metadata_type",&mut field.kind,&["string","number","datetime"]);text_field(ui,"Description",&mut field.description);if ui.button("Remove schema field").clicked(){remove=Some(index);}ui.checkbox(&mut field.touched,"Include in metadata patch");ui.checkbox(&mut field.remove,"Remove value");if ui.text_edit_singleline(&mut field.value).changed(){field.touched=true;}});});}if let Some(index)=remove{self.fields.remove(index);}if ui.button("Add metadata schema field").clicked(){self.fields.push(MetadataField{kind:"string".into(),..Default::default()});}if ui.button("Save metadata schema…").clicked(){let fields:Vec<Value>=self.fields.iter().map(|f|json!({"name":f.name,"label":f.label,"type":f.kind,"description":f.description})).collect();self.review.prepare("save_registry","Replace metadata schema with optimistic revision check",ApiRequest::mutation(Method::PUT,&["api","v1","knowledge",&self.bound,"metadata-schema"],Some(json!({"expected_revision":revision,"fields":fields}))));}
  if ui.add_enabled(!self.loaded_doc.is_empty()&&self.loaded_doc==self.document_id,egui::Button::new("Save document metadata…")).clicked(){match self.metadata_body(){Ok(body)=>self.review.prepare("save_metadata","Patch metadata on this document",ApiRequest::mutation(Method::PATCH,&["api","v1","knowledge",&self.bound,"documents",&self.loaded_doc],Some(body))),Err(error)=>self.job.status=error}}
  ui.label("Batch document IDs, one per line");ui.text_edit_multiline(&mut self.batch_ids);if ui.add_enabled(!lines(&self.batch_ids).is_empty(),egui::Button::new("Apply metadata to selected document IDs…")).clicked(){match self.metadata_body(){Ok(mut body)=>{body["document_ids"]=json!(lines(&self.batch_ids));self.review.prepare("batch_metadata","Patch metadata on these document IDs; inspect partial results",ApiRequest::mutation(Method::PATCH,&["api","v1","knowledge",&self.bound,"documents","metadata","batch"],Some(body)));},Err(error)=>self.job.status=error}}
 }});
    }
    fn segments_ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        egui::CollapsingHeader::new("Segment editor").show(ui,|ui|{text_field(ui,"Search segment text",&mut self.segment_search);ui.add(egui::DragValue::new(&mut self.segment_offset).prefix("Segment offset "));if ui.button("Load segments").clicked(){let mut r=ApiRequest::get(&["api","v1","knowledge",&self.bound,"segments"]).query("limit",100).query("offset",self.segment_offset);if !self.document_id.is_empty(){r=r.query("document_id",&self.document_id);}if !self.segment_search.is_empty(){r=r.query("q",&self.segment_search);}self.job.request("load_segments",api,r,runtime,ctx);}if let Some(rows)=array(&self.segments,"items"){for row in rows{let id=row["segment_id"].as_str().or_else(||row["id"].as_str()).unwrap_or("");if ui.selectable_label(self.segment_id==id,id).clicked(){self.segment_id=id.into();self.segment_text=row["text"].as_str().unwrap_or("").into();self.segment_answer=row["answer"].as_str().unwrap_or("").into();self.segment_keywords=string_list(&row["keywords"]);self.segment_enabled=row["enabled"].as_bool().unwrap_or(true);self.review.clear();}}}ui.label(format!("Loaded segment: {}",self.segment_id));ui.label("Text");ui.add(egui::TextEdit::multiline(&mut self.segment_text).desired_rows(5));ui.label("Answer");ui.text_edit_multiline(&mut self.segment_answer);ui.label("Keywords, one per line");ui.text_edit_multiline(&mut self.segment_keywords);ui.checkbox(&mut self.segment_enabled,"Segment enabled");if !self.segment_id.is_empty(){let keywords=lines(&self.segment_keywords);let valid=!self.segment_text.is_empty()&&self.segment_text.chars().count()<=200000&&self.segment_answer.chars().count()<=200000&&keywords.len()<=100&&keywords.iter().all(|v|v.chars().count()<=256);ui.horizontal_wrapped(|ui|{if ui.add_enabled(valid,egui::Button::new("Save segment…")).clicked(){self.review.prepare("save_segment","Replace segment text, answer and keywords",ApiRequest::mutation(Method::PUT,&["api","v1","knowledge",&self.bound,"segments",&self.segment_id],Some(json!({"text":self.segment_text,"answer":self.segment_answer,"keywords":keywords}))));}if ui.button("Apply segment status…").clicked(){self.review.prepare("segment_status","Change segment retrieval availability",ApiRequest::mutation(Method::PATCH,&["api","v1","knowledge",&self.bound,"segments",&self.segment_id,"status"],Some(json!({"enabled":self.segment_enabled}))));}if ui.button("Delete segment…").clicked(){self.review.prepare("delete_segment","Delete this segment",ApiRequest::mutation(Method::DELETE,&["api","v1","knowledge",&self.bound,"segments",&self.segment_id],None));}});}ui.label("Batch segment IDs, one per line");ui.text_edit_multiline(&mut self.segment_ids);let ids=lines(&self.segment_ids);if ui.add_enabled(!ids.is_empty()&&ids.len()<=500,egui::Button::new("Apply status to segment IDs…")).clicked(){self.review.prepare("batch_segments","Set segment status; partial updates will be reported",ApiRequest::mutation(Method::POST,&["api","v1","knowledge",&self.bound,"segments","batch","enable"],Some(json!({"segment_ids":ids,"enabled":self.segment_enabled}))));}});
    }
    fn config_ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        egui::CollapsingHeader::new("Chunking and retrieval settings").show(ui,|ui|{if ui.button("Load dataset configuration").clicked(){self.job.request("load_config",api,ApiRequest::get(&["api","v1","knowledge",&self.bound,"config"]),runtime,ctx);}if self.config.is_object(){select_text(ui,"chunk_mode",&mut self.chunk_mode,&["automatic","separator","heading","paragraph","hierarchical","qa"]);ui.add(egui::DragValue::new(&mut self.chunk_size).range(1..=200000).prefix("Chunk size "));ui.add(egui::DragValue::new(&mut self.chunk_overlap).range(0..=200000).prefix("Overlap "));ui.label("Complete stored chunking object. Known API fields round-trip; regex is unsupported.");ui.add(egui::TextEdit::multiline(&mut self.chunk_advanced).code_editor().desired_rows(6));if ui.button("Save complete chunking config…").clicked(){match self.chunk_body(){Ok(body)=>self.review.prepare("save_chunking","Replace complete chunking config; existing documents need reindexing",ApiRequest::mutation(Method::PUT,&["api","v1","knowledge",&self.bound,"config"],Some(json!({"chunking_config":body})))),Err(error)=>self.job.status=error}}ui.label("Retrieval recursive patch (nested fusion/rerank/mmr objects; omitted fields stay unchanged)");ui.add(egui::TextEdit::multiline(&mut self.retrieval_patch).code_editor().desired_rows(5));if ui.button("Patch retrieval configuration…").clicked(){match json_object(&self.retrieval_patch){Ok(body)=>self.review.prepare("save_retrieval","Patch retrieval settings",ApiRequest::mutation(Method::PUT,&["api","v1","knowledge",&self.bound,"config"],Some(json!({"retrieval_config":body})))),Err(_)=>self.job.status="Retrieval patch must be an object".into()}}ui.label("Chunk preview text");ui.add(egui::TextEdit::multiline(&mut self.preview_text).desired_rows(3));if ui.add_enabled(!self.preview_text.is_empty(),egui::Button::new("Preview proposed chunking")).clicked(){match self.chunk_body(){Ok(body)=>self.job.request("preview_chunking",api,ApiRequest::mutation(Method::POST,&["api","v1","knowledge",&self.bound,"chunk","preview"],Some(json!({"text":self.preview_text,"config":body}))),runtime,ctx),Err(error)=>self.job.status=error}}}});
    }
    fn batch_ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        egui::CollapsingHeader::new("Durable batch operations").show(ui,|ui|{ui.label("Document IDs, one per line");ui.text_edit_multiline(&mut self.batch_ids);ui.checkbox(&mut self.all_documents,"Reindex all documents in this dataset");let ids=lines(&self.batch_ids);if ui.add_enabled(!self.batch_unknown&&(self.all_documents||(!ids.is_empty()&&ids.len()<=100)),egui::Button::new("Reindex documents…")).clicked(){self.review.prepare("batch_reindex","Start durable document reindex batch",ApiRequest::mutation(Method::POST,&["api","v1","knowledge",&self.bound,"documents","batch-reindex"],Some(json!({"document_ids":if self.all_documents{Vec::<String>::new()}else{ids.clone()},"all_documents":self.all_documents}))));}if ui.add_enabled(!self.batch_unknown&&!ids.is_empty()&&ids.len()<=100,egui::Button::new("Delete selected document IDs…")).clicked(){self.review.prepare("batch_delete","Delete exactly these document IDs",ApiRequest::mutation(Method::POST,&["api","v1","knowledge",&self.bound,"documents","batch-delete"],Some(json!({"document_ids":ids}))));}ui.label("At most 100 explicit document IDs per reviewed request; partial receipts remain visible.");if self.batch_unknown{ui.label("The previous batch has not been reconciled. Resume its receipt; do not submit it again.");if self.batch_id.is_empty()&&ui.button("I inspected the original submission; release unknown-outcome guard").clicked(){self.batch_unknown=false;self.job.outcome_unknown=false;}}text_field(ui,"Batch operation ID",&mut self.batch_id);if ui.add_enabled(!self.batch_id.is_empty(),egui::Button::new("Resume batch status")).clicked(){self.job.request("load_batch",api,ApiRequest::get(&["api","v1","knowledge",&self.bound,"document-batches",&self.batch_id]),runtime,ctx);}if self.next_batch_poll.is_some()&&ui.button("Detach batch observation").clicked(){self.next_batch_poll=None;}if !self.batch.is_null(){ui.label("A partial or failed batch is not reported as all documents succeeded.");json_view(ui,&self.batch);}});
    }
}
